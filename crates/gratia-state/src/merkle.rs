//! Merkle tree implementation for state roots and inclusion proofs.
//!
//! Used for:
//! - Block transaction roots (proving a transaction is in a block)
//! - State roots (proving account state at a given block)
//! - Attestation roots (proving a PoL attestation is in a block)
//!
//! This implementation uses SHA-256 as the hash function, consistent with
//! the rest of the Gratia protocol. It builds a standard binary Merkle tree
//! and supports generating and verifying inclusion proofs.

use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};

// ============================================================================
// Merkle Proof
// ============================================================================

/// Direction of a sibling node in a Merkle proof path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofDirection {
    /// The sibling is to the left (the proven leaf is on the right).
    Left,
    /// The sibling is to the right (the proven leaf is on the left).
    Right,
}

/// A single step in a Merkle inclusion proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofStep {
    /// Hash of the sibling node at this level.
    pub hash: [u8; 32],
    /// Whether the sibling is to the left or right.
    pub direction: ProofDirection,
}

/// A Merkle inclusion proof demonstrating that a specific leaf is part of a tree
/// with a known root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    /// The leaf hash being proven.
    pub leaf_hash: [u8; 32],
    /// Path of sibling hashes from leaf to root.
    pub path: Vec<ProofStep>,
    /// The expected root hash.
    pub root: [u8; 32],
}

// ============================================================================
// Merkle Tree
// ============================================================================

/// A binary Merkle tree built from a list of leaf hashes.
///
/// The tree is stored as a flat vector of levels, where level 0 contains the
/// leaves and the last level contains the root. Odd-numbered levels duplicate
/// the last node to maintain a balanced binary structure.
pub struct MerkleTree {
    /// Each element is a level of the tree. Index 0 = leaves, last = root.
    levels: Vec<Vec<[u8; 32]>>,
}

impl MerkleTree {
    /// Build a Merkle tree from a list of leaf hashes.
    ///
    /// If the input is empty, the tree has a single all-zeros root
    /// (representing an empty set). If there is one leaf, the root
    /// equals that leaf's hash.
    pub fn build(leaves: &[[u8; 32]]) -> Self {
        if leaves.is_empty() {
            return MerkleTree {
                levels: vec![vec![[0u8; 32]]],
            };
        }

        let mut levels: Vec<Vec<[u8; 32]>> = Vec::new();
        levels.push(leaves.to_vec());

        let mut current = leaves.to_vec();
        while current.len() > 1 {
            let mut next = Vec::with_capacity((current.len() + 1) / 2);
            for chunk in current.chunks(2) {
                if chunk.len() == 2 {
                    next.push(hash_pair(&chunk[0], &chunk[1]));
                } else {
                    // WHY: Duplicating the last node for odd-count levels is the standard
                    // Merkle tree approach used by Bitcoin and most blockchains. This ensures
                    // the tree is always a complete binary tree.
                    next.push(hash_pair(&chunk[0], &chunk[0]));
                }
            }
            levels.push(next.clone());
            current = next;
        }

        MerkleTree { levels }
    }

    /// Build a Merkle tree from raw data items (hashes each item first).
    pub fn build_from_data(items: &[&[u8]]) -> Self {
        let leaves: Vec<[u8; 32]> = items.iter().map(|data| sha256_hash(data)).collect();
        Self::build(&leaves)
    }

    /// Get the Merkle root hash.
    pub fn root(&self) -> [u8; 32] {
        self.levels
            .last()
            .and_then(|level| level.first().copied())
            .unwrap_or([0u8; 32])
    }

    /// Get the number of leaves in the tree.
    pub fn leaf_count(&self) -> usize {
        self.levels.first().map(|l| l.len()).unwrap_or(0)
    }

    /// Get the depth of the tree (number of levels minus 1).
    pub fn depth(&self) -> usize {
        if self.levels.is_empty() {
            0
        } else {
            self.levels.len() - 1
        }
    }

    /// Generate a Merkle inclusion proof for the leaf at the given index.
    ///
    /// Returns `None` if the index is out of bounds or the tree is empty.
    pub fn generate_proof(&self, leaf_index: usize) -> Option<MerkleProof> {
        let leaves = self.levels.first()?;
        // WHY: An empty tree has a single zero node as sentinel. Generating a
        // proof for it would create a false positive (proving a non-existent
        // leaf exists). Reject proofs for empty trees.
        if leaves.len() == 1 && leaves[0] == [0u8; 32] {
            return None;
        }
        if leaf_index >= leaves.len() {
            return None;
        }

        let leaf_hash = leaves[leaf_index];
        let mut path = Vec::new();
        let mut index = leaf_index;

        // Walk up from the leaf level to one below the root.
        for level in &self.levels[..self.levels.len().saturating_sub(1)] {
            let sibling_index = if index % 2 == 0 {
                // Current node is left child; sibling is to the right.
                if index + 1 < level.len() {
                    index + 1
                } else {
                    // WHY: When the level has an odd count, the last node's sibling
                    // is itself (it was duplicated during tree construction).
                    index
                }
            } else {
                // Current node is right child; sibling is to the left.
                index - 1
            };

            let direction = if index % 2 == 0 {
                ProofDirection::Right
            } else {
                ProofDirection::Left
            };

            path.push(ProofStep {
                hash: level[sibling_index],
                direction,
            });

            // Move to the parent index in the next level.
            index /= 2;
        }

        Some(MerkleProof {
            leaf_hash,
            path,
            root: self.root(),
        })
    }
}

// ============================================================================
// Verification (standalone, does not require the full tree)
// ============================================================================

/// Verify a Merkle inclusion proof.
///
/// Returns `true` if the proof is valid: the leaf, combined with the proof path,
/// produces the expected root hash.
pub fn verify_proof(proof: &MerkleProof) -> bool {
    let mut current = proof.leaf_hash;

    for step in &proof.path {
        current = match step.direction {
            ProofDirection::Left => hash_pair(&step.hash, &current),
            ProofDirection::Right => hash_pair(&current, &step.hash),
        };
    }

    current == proof.root
}

// ============================================================================
// Hash Helpers
// ============================================================================

/// Hash two 32-byte nodes together to produce a parent hash.
fn hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// SHA-256 hash of arbitrary data.
fn sha256_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(data: &[u8]) -> [u8; 32] {
        sha256_hash(data)
    }

    #[test]
    fn test_empty_tree() {
        let tree = MerkleTree::build(&[]);
        assert_eq!(tree.root(), [0u8; 32]);
        assert_eq!(tree.leaf_count(), 1); // Single zero node
    }

    #[test]
    fn test_single_leaf() {
        let h = leaf(b"only-leaf");
        let tree = MerkleTree::build(&[h]);
        assert_eq!(tree.root(), h);
        assert_eq!(tree.leaf_count(), 1);
    }

    #[test]
    fn test_two_leaves() {
        let a = leaf(b"leaf-a");
        let b = leaf(b"leaf-b");
        let tree = MerkleTree::build(&[a, b]);

        let expected_root = hash_pair(&a, &b);
        assert_eq!(tree.root(), expected_root);
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(tree.depth(), 1);
    }

    #[test]
    fn test_three_leaves_odd_count() {
        let a = leaf(b"a");
        let b = leaf(b"b");
        let c = leaf(b"c");
        let tree = MerkleTree::build(&[a, b, c]);

        // Level 0: [a, b, c]
        // Level 1: [hash(a,b), hash(c,c)]
        // Level 2: [hash(hash(a,b), hash(c,c))]
        let ab = hash_pair(&a, &b);
        let cc = hash_pair(&c, &c);
        let expected = hash_pair(&ab, &cc);
        assert_eq!(tree.root(), expected);
    }

    #[test]
    fn test_four_leaves() {
        let a = leaf(b"a");
        let b = leaf(b"b");
        let c = leaf(b"c");
        let d = leaf(b"d");
        let tree = MerkleTree::build(&[a, b, c, d]);

        let ab = hash_pair(&a, &b);
        let cd = hash_pair(&c, &d);
        let expected = hash_pair(&ab, &cd);
        assert_eq!(tree.root(), expected);
        assert_eq!(tree.depth(), 2);
    }

    #[test]
    fn test_deterministic() {
        let leaves: Vec<[u8; 32]> = (0..10u8).map(|i| leaf(&[i])).collect();
        let tree1 = MerkleTree::build(&leaves);
        let tree2 = MerkleTree::build(&leaves);
        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    fn test_different_leaves_different_root() {
        let a = MerkleTree::build(&[leaf(b"x"), leaf(b"y")]);
        let b = MerkleTree::build(&[leaf(b"y"), leaf(b"x")]);
        // Order matters
        assert_ne!(a.root(), b.root());
    }

    #[test]
    fn test_build_from_data() {
        let tree = MerkleTree::build_from_data(&[b"tx1", b"tx2", b"tx3"]);
        assert_eq!(tree.leaf_count(), 3);
        assert_ne!(tree.root(), [0u8; 32]);
    }

    // --- Proof tests ---

    #[test]
    fn test_proof_two_leaves() {
        let a = leaf(b"a");
        let b = leaf(b"b");
        let tree = MerkleTree::build(&[a, b]);

        let proof0 = tree.generate_proof(0).unwrap();
        assert_eq!(proof0.leaf_hash, a);
        assert!(verify_proof(&proof0));

        let proof1 = tree.generate_proof(1).unwrap();
        assert_eq!(proof1.leaf_hash, b);
        assert!(verify_proof(&proof1));
    }

    #[test]
    fn test_proof_four_leaves() {
        let leaves: Vec<[u8; 32]> = (0..4u8).map(|i| leaf(&[i])).collect();
        let tree = MerkleTree::build(&leaves);

        for i in 0..4 {
            let proof = tree.generate_proof(i).unwrap();
            assert_eq!(proof.leaf_hash, leaves[i]);
            assert!(verify_proof(&proof));
        }
    }

    #[test]
    fn test_proof_odd_count() {
        let leaves: Vec<[u8; 32]> = (0..5u8).map(|i| leaf(&[i])).collect();
        let tree = MerkleTree::build(&leaves);

        for i in 0..5 {
            let proof = tree.generate_proof(i).unwrap();
            assert!(verify_proof(&proof), "proof failed for index {}", i);
        }
    }

    #[test]
    fn test_proof_large_tree() {
        let leaves: Vec<[u8; 32]> = (0..100u8).map(|i| leaf(&[i])).collect();
        let tree = MerkleTree::build(&leaves);

        // Verify a sampling of proofs
        for &i in &[0, 1, 49, 50, 98, 99] {
            let proof = tree.generate_proof(i).unwrap();
            assert!(verify_proof(&proof), "proof failed for index {}", i);
        }
    }

    #[test]
    fn test_proof_out_of_bounds() {
        let tree = MerkleTree::build(&[leaf(b"a"), leaf(b"b")]);
        assert!(tree.generate_proof(2).is_none());
        assert!(tree.generate_proof(100).is_none());
    }

    #[test]
    fn test_proof_tampered() {
        let leaves: Vec<[u8; 32]> = (0..4u8).map(|i| leaf(&[i])).collect();
        let tree = MerkleTree::build(&leaves);

        let mut proof = tree.generate_proof(0).unwrap();
        // Tamper with the root
        proof.root[0] ^= 0xFF;
        assert!(!verify_proof(&proof));
    }

    #[test]
    fn test_proof_wrong_leaf() {
        let leaves: Vec<[u8; 32]> = (0..4u8).map(|i| leaf(&[i])).collect();
        let tree = MerkleTree::build(&leaves);

        let mut proof = tree.generate_proof(0).unwrap();
        // Replace leaf with a different hash
        proof.leaf_hash = leaf(b"fake");
        assert!(!verify_proof(&proof));
    }

    #[test]
    fn test_single_leaf_proof() {
        let h = leaf(b"only");
        let tree = MerkleTree::build(&[h]);
        let proof = tree.generate_proof(0).unwrap();
        assert_eq!(proof.leaf_hash, h);
        assert_eq!(proof.root, h);
        assert!(verify_proof(&proof));
    }
}
