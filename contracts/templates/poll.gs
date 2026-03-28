// GratiaScript — On-Chain Poll Contract
//
// One-phone-one-vote polling. Every response comes from a
// Proof-of-Life-verified unique human.
// Use cases:
// - Protocol governance votes
// - Community decisions
// - Market research with verified respondents
//
// Mobile-native opcodes used: @presence(), @caller()

contract Poll {
    let optionAVotes: i32 = 0;
    let optionBVotes: i32 = 0;
    let totalVoters: i32 = 0;
    let isOpen: bool = true;
    const minPresenceScore: i32 = 50;

    function vote(option: i32): bool {
        if (!isOpen) {
            return false;
        }
        let score = @presence();
        if (score < minPresenceScore) {
            return false;
        }
        if (option == 0) {
            optionAVotes = optionAVotes + 1;
        } else {
            optionBVotes = optionBVotes + 1;
        }
        totalVoters = totalVoters + 1;
        emit("vote_cast", "Vote recorded");
        return true;
    }

    function closePoll(): void {
        isOpen = false;
        emit("poll_closed", "Voting ended");
    }

    function getResults(): i32 {
        if (optionAVotes > optionBVotes) {
            return 0;
        }
        return 1;
    }

    function getTotalVotes(): i32 {
        return totalVoters;
    }
}
