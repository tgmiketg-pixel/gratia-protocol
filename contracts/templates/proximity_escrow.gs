// GratiaScript — Proximity Escrow Contract
//
// Releases funds only when enough peers are physically nearby.
// Use cases:
// - In-person deal settlement (both parties present)
// - Group event verification (minimum attendees)
// - Physical meetup proof
//
// Mobile-native opcodes used: @proximity(), @presence()

contract ProximityEscrow {
    let minPeers: i32 = 2;
    let minPresenceScore: i32 = 60;
    let escrowAmount: i64 = 0;
    let isLocked: bool = true;

    function deposit(amount: i64): void {
        escrowAmount = amount;
        isLocked = true;
    }

    function setRequirements(peers: i32, score: i32): void {
        minPeers = peers;
        minPresenceScore = score;
    }

    function tryRelease(): bool {
        let peers = @proximity();
        let score = @presence();
        if (peers >= minPeers && score >= minPresenceScore) {
            isLocked = false;
            emit("released", "Escrow conditions met");
            return true;
        }
        return false;
    }

    function isReleased(): bool {
        return !isLocked;
    }

    function getAmount(): i64 {
        return escrowAmount;
    }
}
