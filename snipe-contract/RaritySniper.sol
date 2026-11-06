// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title RaritySniper
/// @notice Batches multiple marketplace fills into one transaction so they land
///         in a single block, paying a validator tip for inclusion.
/// @dev Keep-winners semantics: a fill that reverts (e.g. a listing sniped a
///      block earlier) is *skipped* (its ETH is retained and refunded) instead
///      of reverting the whole batch, so one stale listing can't wipe out the
///      other buys. The batch reverts only if *every* order fails, so a bundle
///      that would buy nothing is dropped cleanly (no wasted tip).
contract RaritySniper {
    error ArrayLengthMismatch();
    error NoOrdersFilled();
    error FailedToPayValidator();
    error Unauthorized();

    /// @notice Emitted once per order that filled, in call order, so the
    ///         off-chain caller can map results back to its targets/ranks.
    event OrderFilled(uint256 index);

    address public owner;

    constructor() {
        owner = msg.sender;
    }

    modifier onlyOwner() {
        // Also the reentrancy guard: snipe() makes value calls to owner-supplied
        // targets, but any re-entry would arrive with msg.sender != owner.
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    receive() external payable {}

    /// @notice Execute each (target, calldata, value) in order, keeping the
    ///         successes and skipping failures.
    /// @dev `msg.value` must cover Σ values + validatorTip. A reverting sub-call
    ///      returns its value to this contract, so only filled orders actually
    ///      spend. The remainder (plus any marketplace overpayment refund) is
    ///      swept back to the owner at the end.
    /// @param contractAddresses Per-order call target (e.g. Seaport).
    /// @param calls             Per-order calldata (e.g. fulfillAdvancedOrder).
    /// @param values            Per-order msg.value to forward.
    /// @param validatorTip      Wei paid to block.coinbase for inclusion.
    function snipe(
        address[] calldata contractAddresses,
        bytes[] calldata calls,
        uint256[] calldata values,
        uint256 validatorTip
    ) external payable onlyOwner {
        uint256 n = contractAddresses.length;
        if (n != calls.length || n != values.length) revert ArrayLengthMismatch();

        // Balance attributable to *this* call, so the refund at the end never
        // touches any pre-existing contract balance.
        uint256 startBalance = address(this).balance - msg.value;

        uint256 filled;
        for (uint256 i = 0; i < n;) {
            (bool success,) = contractAddresses[i].call{value: values[i]}(calls[i]);
            if (success) {
                emit OrderFilled(i);
                unchecked { ++filled; }
            }
            unchecked { ++i; }
        }

        // Nothing bought → revert so the builder drops the bundle (tip unpaid)
        // instead of landing a no-op that burns gas.
        if (filled == 0) revert NoOrdersFilled();

        // At least one order filled, so the tip that won us inclusion is earned.
        if (validatorTip > 0) {
            (bool paid,) = block.coinbase.call{value: validatorTip}("");
            if (!paid) revert FailedToPayValidator();
        }

        // Sweep this call's unspent wei (skipped orders' value + any marketplace
        // overpayment refund) back to the owner. Refund failure is non-fatal
        // (the ETH stays recoverable via withdraw()), so an owner-side quirk can
        // never undo the successful buys. Guarded to avoid an underflow revert.
        uint256 bal = address(this).balance;
        if (bal > startBalance) {
            (bool refunded,) = payable(owner).call{value: bal - startBalance}("");
            refunded; // intentionally ignored, see above
        }
    }

    /// @notice Send the entire contract balance to the owner. Recovery path for
    ///         ETH left behind when the refund at the end of snipe() fails.
    function withdraw() external onlyOwner {
        (bool success,) = payable(msg.sender).call{value: address(this).balance}("");
        require(success);
    }
}
