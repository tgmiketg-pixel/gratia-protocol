// GratiaScript — Presence Verification Contract
//
// Gates actions behind a minimum Presence Score.
// Use cases:
// - KYC-lite verification (prove you're a real human)
// - Access control for premium features
// - Spam prevention (bots have score 0)
//
// Mobile-native opcodes used: @presence(), @blockHeight()

contract PresenceVerifier {
    const minScore: i32 = 70;

    function verify(): bool {
        let score = @presence();
        if (score >= minScore) {
            emit("verified", "Presence score sufficient");
            return true;
        }
        return false;
    }

    function getScore(): i32 {
        return @presence();
    }

    function getMinimum(): i32 {
        return minScore;
    }

    function getBlockHeight(): i64 {
        return @blockHeight();
    }
}
