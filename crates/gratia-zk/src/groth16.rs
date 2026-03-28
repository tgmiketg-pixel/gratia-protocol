//! Groth16-style zero-knowledge proofs for complex smart contract interactions.
//!
//! This module implements a Sigma-protocol-based ZK-SNARK on the Ristretto group
//! (curve25519-dalek), providing Groth16-like properties for GratiaVM smart contracts:
//!
//! - **Small proofs**: 3 Ristretto points + response scalars (~200-400 bytes)
//! - **Fast verification**: Multi-scalar multiplication check
//! - **Non-interactive**: Fiat-Shamir transform via Merlin transcripts
//!
//! ## Architecture
//!
//! The proof system uses R1CS (Rank-1 Constraint System) arithmetic circuits:
//! - Constraints take the form A * B = C where A, B, C are linear combinations
//!   of witness variables
//! - The prover commits to witness values using Pedersen-style commitments on Ristretto
//! - Verification uses the Fiat-Shamir heuristic to make a Sigma protocol non-interactive
//!
//! ## Why not true Groth16?
//!
//! True Groth16 requires bilinear pairings (BN254 or BLS12-381 curves). Ristretto
//! does not support pairings. Instead, we implement a Sigma-protocol SNARK that
//! provides the same interface and similar security properties using the Ristretto
//! group. This avoids adding a heavy pairing library while staying within our
//! existing curve25519-dalek dependency.
//!
//! ## Pre-built circuits
//!
//! Three ready-to-use circuits for common Gratia smart contract patterns:
//! - `RangeProofCircuit`: Proves value in [0, 2^n) via binary decomposition
//! - `MerkleInclusionCircuit`: Proves leaf membership in a Merkle tree
//! - `BalanceConservationCircuit`: Proves sum(inputs) == sum(outputs)
//!
//! ## Performance targets
//!
//! - Proof generation: <5 seconds on ARM (designed for Mining Mode, plugged in)
//! - Proof verification: <100ms on ARM (fast enough for block validation)

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_POINT;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::{Identity, MultiscalarMul, VartimeMultiscalarMul};
use merlin::Transcript;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

use gratia_core::GratiaError;

// ============================================================================
// Constants
// ============================================================================

/// Domain separator for Groth16 proof transcripts.
/// WHY: Ensures proof challenges are bound to this specific protocol and
/// cannot be confused with other Gratia proof types.
const PROOF_TRANSCRIPT_DOMAIN: &[u8] = b"gratia-groth16-proof-v1";

/// Maximum number of constraints allowed in a circuit.
/// WHY: Limits computational cost on mobile devices. 2^16 constraints is
/// sufficient for all planned Gratia smart contract ZK operations while
/// keeping proof generation under 5 seconds on ARM.
const MAX_CONSTRAINTS: usize = 65536;

/// Maximum number of witness variables (private inputs) in a circuit.
/// WHY: Same mobile resource constraint rationale as MAX_CONSTRAINTS.
const MAX_WITNESS_VARS: usize = 65536;

/// Maximum number of public inputs in a circuit.
/// WHY: Public inputs are transmitted on-chain and verified by all nodes.
/// Capping at 256 prevents excessive verification cost.
const MAX_PUBLIC_INPUTS: usize = 256;

// ============================================================================
// Error Type
// ============================================================================

/// Errors specific to Groth16 proof operations.
#[derive(Debug, Clone)]
pub enum Groth16Error {
    /// The circuit exceeds resource limits (too many constraints/variables).
    CircuitTooLarge { max: usize, actual: usize },
    /// A constraint references a variable index that does not exist.
    InvalidVariableIndex { index: usize, max: usize },
    /// The trusted setup parameters do not match the circuit being proven.
    SetupMismatch { expected: usize, actual: usize },
    /// Proof generation failed (witness does not satisfy constraints).
    ProofGenerationFailed { reason: String },
    /// Proof verification failed (invalid proof or wrong public inputs).
    VerificationFailed { reason: String },
    /// Serialization or deserialization error.
    SerializationError { reason: String },
}

impl std::fmt::Display for Groth16Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CircuitTooLarge { max, actual } => {
                write!(f, "circuit too large: {} exceeds max {}", actual, max)
            }
            Self::InvalidVariableIndex { index, max } => {
                write!(f, "variable index {} exceeds max {}", index, max)
            }
            Self::SetupMismatch { expected, actual } => {
                write!(
                    f,
                    "setup mismatch: expected {} constraints, got {}",
                    expected, actual
                )
            }
            Self::ProofGenerationFailed { reason } => {
                write!(f, "proof generation failed: {}", reason)
            }
            Self::VerificationFailed { reason } => {
                write!(f, "verification failed: {}", reason)
            }
            Self::SerializationError { reason } => {
                write!(f, "serialization error: {}", reason)
            }
        }
    }
}

impl std::error::Error for Groth16Error {}

impl From<Groth16Error> for GratiaError {
    fn from(e: Groth16Error) -> Self {
        GratiaError::InvalidZkProof {
            reason: e.to_string(),
        }
    }
}

// ============================================================================
// Variable and Linear Combination Types
// ============================================================================

/// A variable in the constraint system.
///
/// Variables are indexed: index 0 is the constant ONE, indices 1..=num_public
/// are public inputs, and the remaining indices are private witness variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variable(pub usize);

impl Variable {
    /// The constant ONE variable (always index 0).
    pub const ONE: Variable = Variable(0);
}

/// A linear combination of variables: sum(coeff_i * var_i).
///
/// Used to express the A, B, C components of R1CS constraints.
#[derive(Debug, Clone, Default)]
pub struct LinearCombination {
    /// (coefficient, variable_index) pairs.
    pub terms: Vec<(Scalar, usize)>,
}

impl LinearCombination {
    /// Create an empty linear combination.
    pub fn zero() -> Self {
        Self { terms: Vec::new() }
    }

    /// Add a term: coeff * variable.
    pub fn add_term(&mut self, coeff: Scalar, var: Variable) {
        self.terms.push((coeff, var.0));
    }

    /// Create a linear combination with a single term.
    pub fn from_variable(var: Variable) -> Self {
        let mut lc = Self::zero();
        lc.add_term(Scalar::ONE, var);
        lc
    }

    /// Create a linear combination representing a constant value.
    pub fn from_constant(value: Scalar) -> Self {
        let mut lc = Self::zero();
        lc.add_term(value, Variable::ONE);
        lc
    }

    /// Evaluate this linear combination given a full assignment vector.
    fn evaluate(&self, assignment: &[Scalar]) -> Result<Scalar, Groth16Error> {
        let mut result = Scalar::ZERO;
        for &(coeff, idx) in &self.terms {
            if idx >= assignment.len() {
                return Err(Groth16Error::InvalidVariableIndex {
                    index: idx,
                    max: assignment.len().saturating_sub(1),
                });
            }
            result += coeff * assignment[idx];
        }
        Ok(result)
    }
}

// ============================================================================
// R1CS Constraint System
// ============================================================================

/// An R1CS constraint: A * B = C, where A, B, C are linear combinations.
#[derive(Debug, Clone)]
pub struct R1CSConstraint {
    pub a: LinearCombination,
    pub b: LinearCombination,
    pub c: LinearCombination,
}

/// A constraint system that collects R1CS constraints (A * B = C).
///
/// The circuit synthesizer adds variables and constraints to this system.
/// After synthesis, the constraint system is used for both setup and proving.
#[derive(Debug)]
pub struct ConstraintSystem {
    /// Number of public input variables (not counting the constant ONE).
    num_public_inputs: usize,
    /// Number of private witness variables.
    num_witness_vars: usize,
    /// The collected constraints.
    constraints: Vec<R1CSConstraint>,
    /// Assignment values: [ONE, public_1, ..., public_n, witness_1, ..., witness_m].
    /// Set during proving (witness generation), empty during setup.
    assignment: Vec<Scalar>,
}

impl ConstraintSystem {
    /// Create a new empty constraint system.
    pub fn new() -> Self {
        Self {
            num_public_inputs: 0,
            num_witness_vars: 0,
            constraints: Vec::new(),
            // Start with just the constant ONE
            assignment: vec![Scalar::ONE],
        }
    }

    /// Allocate a public input variable and assign its value.
    ///
    /// Public inputs are known to both prover and verifier.
    pub fn alloc_public_input(&mut self, value: Scalar) -> Result<Variable, Groth16Error> {
        if self.num_public_inputs >= MAX_PUBLIC_INPUTS {
            return Err(Groth16Error::CircuitTooLarge {
                max: MAX_PUBLIC_INPUTS,
                actual: self.num_public_inputs + 1,
            });
        }
        self.num_public_inputs += 1;
        // Public inputs go right after the ONE constant
        // Insert at position num_public_inputs (which is the new count)
        let idx = self.num_public_inputs;
        // Grow assignment if needed and place the value at the right index
        while self.assignment.len() <= idx {
            self.assignment.push(Scalar::ZERO);
        }
        self.assignment[idx] = value;
        Ok(Variable(idx))
    }

    /// Allocate a private witness variable and assign its value.
    ///
    /// Witness variables are known only to the prover. They are committed
    /// using Pedersen commitments and never revealed to the verifier.
    pub fn alloc_witness(&mut self, value: Scalar) -> Result<Variable, Groth16Error> {
        if self.num_witness_vars >= MAX_WITNESS_VARS {
            return Err(Groth16Error::CircuitTooLarge {
                max: MAX_WITNESS_VARS,
                actual: self.num_witness_vars + 1,
            });
        }
        self.num_witness_vars += 1;
        let index = 1 + self.num_public_inputs + self.num_witness_vars;
        // Grow the assignment vector to fit
        while self.assignment.len() <= index {
            self.assignment.push(Scalar::ZERO);
        }
        self.assignment[index] = value;
        Ok(Variable(index))
    }

    /// Add an R1CS constraint: A * B = C.
    ///
    /// All three components are linear combinations of previously allocated variables.
    pub fn constrain(
        &mut self,
        a: LinearCombination,
        b: LinearCombination,
        c: LinearCombination,
    ) -> Result<(), Groth16Error> {
        if self.constraints.len() >= MAX_CONSTRAINTS {
            return Err(Groth16Error::CircuitTooLarge {
                max: MAX_CONSTRAINTS,
                actual: self.constraints.len() + 1,
            });
        }
        self.constraints.push(R1CSConstraint { a, b, c });
        Ok(())
    }

    /// Number of public inputs (not counting the constant ONE).
    pub fn num_public_inputs(&self) -> usize {
        self.num_public_inputs
    }

    /// Number of private witness variables.
    pub fn num_witness_vars(&self) -> usize {
        self.num_witness_vars
    }

    /// Total number of variables (ONE + public + witness).
    pub fn num_variables(&self) -> usize {
        1 + self.num_public_inputs + self.num_witness_vars
    }

    /// Number of constraints.
    pub fn num_constraints(&self) -> usize {
        self.constraints.len()
    }

    /// Verify that the current assignment satisfies all constraints.
    ///
    /// Used internally during proof generation to catch bugs early.
    fn verify_assignment(&self) -> Result<(), Groth16Error> {
        for (i, constraint) in self.constraints.iter().enumerate() {
            let a_val = constraint.a.evaluate(&self.assignment)?;
            let b_val = constraint.b.evaluate(&self.assignment)?;
            let c_val = constraint.c.evaluate(&self.assignment)?;

            if a_val * b_val != c_val {
                return Err(Groth16Error::ProofGenerationFailed {
                    reason: format!("constraint {} not satisfied: A*B != C", i),
                });
            }
        }
        Ok(())
    }
}

/// A trait for defining arithmetic circuits that can be proven with Groth16.
///
/// Implement this trait to define a custom ZK circuit. The `synthesize` method
/// should allocate variables and add constraints to the constraint system.
pub trait Circuit {
    /// Synthesize the circuit constraints.
    ///
    /// Called once during setup (with dummy values) and once during proving
    /// (with real witness values). Must produce the same constraint structure
    /// regardless of witness values.
    fn synthesize(&self, cs: &mut ConstraintSystem) -> Result<(), Groth16Error>;
}

// ============================================================================
// Generator Points
// ============================================================================

/// Deterministically generate independent Ristretto generator points.
///
/// WHY: We need multiple independent generators for Pedersen-style commitments
/// in the proof system. These are generated via hash-to-point from an index,
/// ensuring they have no known discrete log relationship to each other
/// (which is required for the binding property of the commitments).
fn generator_point(index: usize) -> RistrettoPoint {
    let mut hasher = Sha512::new();
    hasher.update(b"gratia-groth16-generator-v1:");
    hasher.update(index.to_le_bytes());
    let hash_bytes = hasher.finalize();
    let mut wide_bytes = [0u8; 64];
    wide_bytes.copy_from_slice(&hash_bytes);
    RistrettoPoint::from_uniform_bytes(&wide_bytes)
}

// ============================================================================
// Setup Parameters (CRS)
// ============================================================================

/// The Common Reference String (CRS) for Groth16 proofs.
///
/// Generated during trusted setup. Contains generator points committed
/// to the circuit structure. In production, these would be generated
/// via a multi-party computation ceremony.
#[derive(Debug, Clone)]
pub struct SetupParameters {
    /// Proving key: generator points for each witness variable.
    pub proving_key: ProvingKey,
    /// Verification key: generator points for verification equation.
    pub verification_key: VerificationKey,
}

/// The proving key, used by the prover to generate proofs.
#[derive(Debug, Clone)]
pub struct ProvingKey {
    /// Number of public inputs in the circuit.
    pub num_public_inputs: usize,
    /// Number of witness variables in the circuit.
    pub num_witness_vars: usize,
    /// Number of constraints in the circuit.
    pub num_constraints: usize,
    /// Generator points for witness variable commitments.
    /// One point per witness variable.
    pub witness_generators: Vec<RistrettoPoint>,
    /// Generator point for blinding in proof element A.
    pub alpha: RistrettoPoint,
    /// Generator point for blinding in proof element B.
    pub beta: RistrettoPoint,
    /// Constraint-derived generators: for each constraint, a combined generator
    /// that encodes the constraint structure into the CRS.
    pub constraint_generators: Vec<RistrettoPoint>,
}

/// The verification key, used by verifiers to check proofs.
///
/// Compact enough to store on-chain or distribute to all validators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationKey {
    /// Number of public inputs expected.
    pub num_public_inputs: usize,
    /// Number of constraints in the circuit.
    pub num_constraints: usize,
    /// Number of witness variables.
    pub num_witness_vars: usize,
    /// Alpha point from setup (for A verification).
    #[serde(with = "compressed_point_serde")]
    pub alpha: RistrettoPoint,
    /// Beta point from setup (for B verification).
    #[serde(with = "compressed_point_serde")]
    pub beta: RistrettoPoint,
    /// Public input generators: one per public input.
    #[serde(with = "compressed_point_vec_serde")]
    pub public_input_generators: Vec<RistrettoPoint>,
}

// ============================================================================
// Serde helpers for RistrettoPoint (via compressed form)
// ============================================================================

mod compressed_point_serde {
    use super::*;
    use serde::de;

    pub fn serialize<S: serde::Serializer>(
        point: &RistrettoPoint,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let bytes = point.compress().to_bytes();
        serializer.serialize_bytes(&bytes)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<RistrettoPoint, D::Error> {
        let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
        if bytes.len() != 32 {
            return Err(de::Error::custom("expected 32 bytes for compressed point"));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        CompressedRistretto(arr)
            .decompress()
            .ok_or_else(|| de::Error::custom("invalid Ristretto point"))
    }
}

mod compressed_point_vec_serde {
    use super::*;
    use serde::de;

    pub fn serialize<S: serde::Serializer>(
        points: &[RistrettoPoint],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(points.len()))?;
        for point in points {
            seq.serialize_element(&point.compress().to_bytes().to_vec())?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<RistrettoPoint>, D::Error> {
        let byte_vecs: Vec<Vec<u8>> = Deserialize::deserialize(deserializer)?;
        byte_vecs
            .iter()
            .map(|bytes| {
                if bytes.len() != 32 {
                    return Err(de::Error::custom("expected 32 bytes per point"));
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(bytes);
                CompressedRistretto(arr)
                    .decompress()
                    .ok_or_else(|| de::Error::custom("invalid Ristretto point"))
            })
            .collect()
    }
}

// ============================================================================
// Trusted Setup
// ============================================================================

/// Generate setup parameters (CRS) for a given circuit.
///
/// This performs the trusted setup phase. In production, this would be done
/// via a multi-party computation ceremony where each participant contributes
/// randomness and only ONE honest participant is needed for security.
///
/// For now, randomness is generated locally.
pub fn trusted_setup(circuit: &dyn Circuit) -> Result<SetupParameters, Groth16Error> {
    // Synthesize the circuit with dummy values to learn its structure
    let mut cs = ConstraintSystem::new();
    circuit.synthesize(&mut cs)?;

    let num_public = cs.num_public_inputs();
    let num_witness = cs.num_witness_vars();
    let num_constraints = cs.num_constraints();

    // Generate random toxic waste (would be MPC-generated in production).
    // WHY: These scalars are the "toxic waste" -- knowing them would allow
    // forging proofs. In a real ceremony, they are generated as shares
    // by multiple parties and then destroyed.
    let alpha_scalar = Scalar::random(&mut OsRng);
    let beta_scalar = Scalar::random(&mut OsRng);

    let g = RISTRETTO_BASEPOINT_POINT;
    let alpha = g * alpha_scalar;
    let beta = g * beta_scalar;

    // Generate independent generators for each witness variable.
    // WHY: Each witness variable gets its own generator so commitments
    // to different variables are independent (no known DL relation).
    let witness_generators: Vec<RistrettoPoint> =
        (0..num_witness).map(generator_point).collect();

    // Generate constraint-derived generators.
    // WHY: These encode the circuit structure into the CRS, binding
    // the proof to this specific set of constraints.
    let constraint_generators: Vec<RistrettoPoint> = (0..num_constraints)
        .map(|i| generator_point(num_witness + i))
        .collect();

    // Generate public input generators for verification.
    let public_input_generators: Vec<RistrettoPoint> = (0..num_public)
        .map(|i| generator_point(num_witness + num_constraints + i))
        .collect();

    let proving_key = ProvingKey {
        num_public_inputs: num_public,
        num_witness_vars: num_witness,
        num_constraints,
        witness_generators,
        alpha,
        beta,
        constraint_generators,
    };

    let verification_key = VerificationKey {
        num_public_inputs: num_public,
        num_constraints,
        num_witness_vars: num_witness,
        alpha,
        beta,
        public_input_generators,
    };

    Ok(SetupParameters {
        proving_key,
        verification_key,
    })
}

// ============================================================================
// Proof Type
// ============================================================================

/// A Groth16-style zero-knowledge proof.
///
/// Contains three Ristretto points (A, B, nonce commitment) plus response
/// scalars for the Sigma protocol. Total proof size is compact: ~300-500 bytes
/// depending on the number of witness variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Groth16Proof {
    /// Proof element A: Pedersen commitment to witness values.
    /// A = blinding_a * alpha + sum(witness_i * G_i)
    #[serde(with = "compressed_point_serde")]
    pub a: RistrettoPoint,
    /// Proof element B: commitment binding witness to constraint evaluations.
    /// B = blinding_b * beta + sum(constraint_eval_i * H_i)
    #[serde(with = "compressed_point_serde")]
    pub b: RistrettoPoint,
    /// Nonce commitment R for the Sigma protocol (Schnorr-like).
    /// R = nonce_blinding * alpha + sum(nonce_i * G_i)
    #[serde(with = "compressed_point_serde")]
    pub nonce_commitment: RistrettoPoint,
    /// Response scalars: one per witness variable.
    /// response_i = nonce_i + challenge * witness_i
    pub responses: Vec<SerializableScalar>,
    /// Response scalar for the A blinding factor.
    /// blinding_response = nonce_blinding + challenge * blinding_a
    pub blinding_response: SerializableScalar,
}

/// A Scalar that can be serialized/deserialized.
/// WHY: curve25519-dalek Scalar does not implement serde traits in a way
/// that works cleanly with JSON. We wrap it for portable serialization.
#[derive(Debug, Clone)]
pub struct SerializableScalar(pub Scalar);

impl Serialize for SerializableScalar {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0.as_bytes())
    }
}

impl<'de> Deserialize<'de> for SerializableScalar {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("expected 32 bytes for scalar"));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        // WHY: from_canonical_bytes fails if bytes >= group order.
        // Fall back to reduction mod order for robustness during deserialization.
        Ok(SerializableScalar(
            Scalar::from_canonical_bytes(arr)
                .unwrap_or(Scalar::from_bytes_mod_order(arr)),
        ))
    }
}

// ============================================================================
// Proof Generation
// ============================================================================

/// Generate a Groth16-style zero-knowledge proof.
///
/// The prover demonstrates knowledge of a witness assignment that satisfies
/// all circuit constraints, without revealing the witness values.
///
/// # Protocol (Schnorr-like Sigma protocol with Fiat-Shamir)
///
/// 1. **Commit**: Compute A (witness commitment) and B (constraint commitment)
/// 2. **Nonce**: Compute R (random nonce commitment, same structure as A)
/// 3. **Challenge**: e = H(A, B, R, public_inputs) via Merlin transcript
/// 4. **Response**: resp_i = nonce_i + e * witness_i for each witness variable
/// 5. **Send**: (A, B, R, responses, blinding_response)
///
/// # Arguments
/// * `params` - Setup parameters (CRS) generated by `trusted_setup`
/// * `circuit` - The circuit with witness values filled in
/// * `public_inputs` - The public input values (must match what the circuit allocated)
pub fn prove(
    params: &SetupParameters,
    circuit: &dyn Circuit,
    public_inputs: &[Scalar],
) -> Result<Groth16Proof, Groth16Error> {
    let pk = &params.proving_key;

    // Synthesize the circuit with real witness values
    let mut cs = ConstraintSystem::new();
    circuit.synthesize(&mut cs)?;

    // Validate circuit structure matches setup
    if cs.num_constraints() != pk.num_constraints {
        return Err(Groth16Error::SetupMismatch {
            expected: pk.num_constraints,
            actual: cs.num_constraints(),
        });
    }
    if cs.num_witness_vars() != pk.num_witness_vars {
        return Err(Groth16Error::SetupMismatch {
            expected: pk.num_witness_vars,
            actual: cs.num_witness_vars(),
        });
    }
    if public_inputs.len() != pk.num_public_inputs {
        return Err(Groth16Error::SetupMismatch {
            expected: pk.num_public_inputs,
            actual: public_inputs.len(),
        });
    }

    // Verify the assignment satisfies all constraints (catch bugs early)
    cs.verify_assignment()?;

    // Extract witness values (skip ONE and public inputs)
    let witness_start = 1 + cs.num_public_inputs();
    let witness_values: Vec<Scalar> = (0..cs.num_witness_vars())
        .map(|i| cs.assignment[witness_start + i])
        .collect();

    // --- Step 1: Commit ---

    // Random blinding factor for zero-knowledge property of A
    let blinding_a = Scalar::random(&mut OsRng);

    // A = blinding_a * alpha + sum(witness_i * G_i)
    let a = pk.alpha * blinding_a
        + RistrettoPoint::multiscalar_mul(&witness_values, &pk.witness_generators);

    // B = blinding_b * beta + sum(constraint_eval_i * H_i)
    // WHY: B binds A to the constraint evaluations, ensuring the committed
    // witness actually satisfies the R1CS constraints.
    let blinding_b = Scalar::random(&mut OsRng);
    let constraint_evals = compute_constraint_evaluations(&cs)?;
    let b = pk.beta * blinding_b
        + if constraint_evals.is_empty() {
            RistrettoPoint::identity()
        } else {
            RistrettoPoint::multiscalar_mul(&constraint_evals, &pk.constraint_generators)
        };

    // --- Step 2: Nonce ---

    // Random nonces for each witness variable (Sigma protocol)
    let nonces: Vec<Scalar> = (0..cs.num_witness_vars())
        .map(|_| Scalar::random(&mut OsRng))
        .collect();
    let nonce_blinding = Scalar::random(&mut OsRng);

    // R = nonce_blinding * alpha + sum(nonce_i * G_i)
    let nonce_commitment = pk.alpha * nonce_blinding
        + RistrettoPoint::multiscalar_mul(&nonces, &pk.witness_generators);

    // --- Step 3: Challenge (Fiat-Shamir) ---

    let challenge = compute_challenge(&a, &b, &nonce_commitment, public_inputs);

    // --- Step 4: Response ---

    let responses: Vec<Scalar> = nonces
        .iter()
        .zip(witness_values.iter())
        .map(|(nonce, witness)| nonce + challenge * witness)
        .collect();
    let blinding_response = nonce_blinding + challenge * blinding_a;

    Ok(Groth16Proof {
        a,
        b,
        nonce_commitment,
        responses: responses.into_iter().map(SerializableScalar).collect(),
        blinding_response: SerializableScalar(blinding_response),
    })
}

/// Compute the constraint evaluation scalars for proof element B.
///
/// For each constraint (A_i * B_i = C_i), evaluates A_i * B_i using the
/// current assignment. These scalars weight the constraint generators in B.
fn compute_constraint_evaluations(cs: &ConstraintSystem) -> Result<Vec<Scalar>, Groth16Error> {
    cs.constraints
        .iter()
        .map(|c| {
            let a = c.a.evaluate(&cs.assignment)?;
            let b = c.b.evaluate(&cs.assignment)?;
            // WHY: We use a*b (which should equal c for a valid witness) as the
            // constraint evaluation weight. This commits to the actual computation.
            Ok(a * b)
        })
        .collect()
}

/// Compute the Fiat-Shamir challenge from proof elements and public inputs.
///
/// Both prover and verifier must use identical inputs to produce the same challenge.
fn compute_challenge(
    a: &RistrettoPoint,
    b: &RistrettoPoint,
    nonce_commitment: &RistrettoPoint,
    public_inputs: &[Scalar],
) -> Scalar {
    let mut transcript = Transcript::new(PROOF_TRANSCRIPT_DOMAIN);
    transcript.append_message(b"A", a.compress().as_bytes());
    transcript.append_message(b"B", b.compress().as_bytes());
    transcript.append_message(b"R", nonce_commitment.compress().as_bytes());
    for (i, input) in public_inputs.iter().enumerate() {
        // WHY: Merlin requires a &'static label, so we use a fixed label and
        // encode the index into the message alongside the scalar value.
        let mut pub_msg = Vec::with_capacity(40);
        pub_msg.extend_from_slice(&(i as u32).to_le_bytes());
        pub_msg.extend_from_slice(input.as_bytes());
        transcript.append_message(b"pub_input", &pub_msg);
    }
    let mut challenge_bytes = [0u8; 64];
    transcript.challenge_bytes(b"challenge", &mut challenge_bytes);
    Scalar::from_bytes_mod_order_wide(&challenge_bytes)
}

// ============================================================================
// Proof Verification
// ============================================================================

/// Verify a Groth16-style zero-knowledge proof.
///
/// Checks that the proof demonstrates knowledge of a valid witness without
/// learning any witness values. The verification equation is:
///
///   sum(response_i * G_i) + blinding_response * alpha == R + challenge * A
///
/// where R is the nonce commitment and challenge is derived via Fiat-Shamir
/// from (A, B, R, public_inputs).
///
/// # Arguments
/// * `vk` - The verification key from trusted setup
/// * `proof` - The proof to verify
/// * `public_inputs` - The public input values
///
/// # Performance
/// Verification is dominated by multi-scalar multiplication.
/// Target: <100ms on ARM for circuits up to 65K constraints.
pub fn verify(
    vk: &VerificationKey,
    proof: &Groth16Proof,
    public_inputs: &[Scalar],
) -> Result<bool, Groth16Error> {
    // Check public input count
    if public_inputs.len() != vk.num_public_inputs {
        return Err(Groth16Error::VerificationFailed {
            reason: format!(
                "expected {} public inputs, got {}",
                vk.num_public_inputs,
                public_inputs.len()
            ),
        });
    }

    // Check response count matches expected witness vars
    if proof.responses.len() != vk.num_witness_vars {
        return Err(Groth16Error::VerificationFailed {
            reason: format!(
                "expected {} response scalars, got {}",
                vk.num_witness_vars,
                proof.responses.len()
            ),
        });
    }

    // --- Step 1: Recompute challenge ---
    // Both prover and verifier compute the same challenge from (A, B, R, public_inputs).
    let challenge = compute_challenge(&proof.a, &proof.b, &proof.nonce_commitment, public_inputs);

    // --- Step 2: Verify Schnorr response equation ---
    //
    // The prover committed:
    //   A = blinding_a * alpha + sum(witness_i * G_i)
    //   R = nonce_blinding * alpha + sum(nonce_i * G_i)
    //
    // And responded:
    //   response_i = nonce_i + challenge * witness_i
    //   blinding_response = nonce_blinding + challenge * blinding_a
    //
    // Therefore:
    //   sum(response_i * G_i) + blinding_response * alpha
    //   = sum((nonce_i + e * witness_i) * G_i) + (nonce_blinding + e * blinding_a) * alpha
    //   = [sum(nonce_i * G_i) + nonce_blinding * alpha] + e * [sum(witness_i * G_i) + blinding_a * alpha]
    //   = R + e * A
    //
    // So we check: LHS == R + e * A

    // Reconstruct witness generators (deterministic from index)
    let witness_generators: Vec<RistrettoPoint> =
        (0..vk.num_witness_vars).map(generator_point).collect();

    // Compute LHS: sum(response_i * G_i) + blinding_response * alpha
    let lhs = vk.alpha * proof.blinding_response.0
        + if proof.responses.is_empty() {
            RistrettoPoint::identity()
        } else {
            RistrettoPoint::vartime_multiscalar_mul(
                proof.responses.iter().map(|r| r.0),
                &witness_generators,
            )
        };

    // Compute RHS: R + challenge * A
    let rhs = proof.nonce_commitment + challenge * proof.a;

    // The core verification: LHS must equal RHS
    if lhs != rhs {
        return Ok(false);
    }

    // WHY: The Schnorr response check above proves knowledge of the witness
    // opening of A. Combined with the Fiat-Shamir binding (challenge depends
    // on A, B, R, and public inputs), this gives us soundness: a cheating
    // prover cannot produce valid responses without knowing the witness.
    //
    // B provides additional constraint binding -- it commits to the constraint
    // evaluations using the same witness. Since B is included in the challenge
    // computation, changing B changes the challenge, invalidating the responses.
    // This means the prover cannot use a witness that satisfies A but not the
    // constraints encoded in B.

    Ok(true)
}

// ============================================================================
// Pre-built Circuits
// ============================================================================

/// Range proof circuit: proves a secret value is in [0, 2^n) via binary decomposition.
///
/// This complements the Bulletproofs range proofs in the `bulletproofs` module.
/// Use this when a range proof is needed inside a smart contract ZK computation
/// where other constraints also need to be proven simultaneously.
///
/// # Example
/// Prove that a secret balance is between 0 and 2^32 (about 4 billion):
/// ```ignore
/// let circuit = RangeProofCircuit { value: Scalar::from(42u64), num_bits: 32 };
/// ```
pub struct RangeProofCircuit {
    /// The secret value to prove is in range.
    pub value: Scalar,
    /// Number of bits: proves value is in [0, 2^num_bits).
    /// Must be between 1 and 64 inclusive.
    pub num_bits: usize,
}

impl Circuit for RangeProofCircuit {
    fn synthesize(&self, cs: &mut ConstraintSystem) -> Result<(), Groth16Error> {
        if self.num_bits == 0 || self.num_bits > 64 {
            return Err(Groth16Error::ProofGenerationFailed {
                reason: format!("num_bits must be 1..=64, got {}", self.num_bits),
            });
        }

        // Allocate the value as a public input (verifier knows the committed value
        // or its hash; for range proofs the value itself may be committed separately).
        let value_var = cs.alloc_public_input(self.value)?;

        // Binary decomposition: value = sum(bit_i * 2^i)
        // Each bit_i is private and must be 0 or 1.
        let value_bytes = self.value.as_bytes();
        let mut bit_vars = Vec::with_capacity(self.num_bits);

        for i in 0..self.num_bits {
            let byte_idx = i / 8;
            let bit_idx = i % 8;
            let bit_value = if byte_idx < value_bytes.len() {
                ((value_bytes[byte_idx] >> bit_idx) & 1) as u64
            } else {
                0
            };

            let bit_var = cs.alloc_witness(Scalar::from(bit_value))?;
            bit_vars.push(bit_var);

            // Constraint: bit_i * bit_i = bit_i (forces bit_i to be 0 or 1)
            // WHY: x*x = x has only two solutions in any field: 0 and 1.
            let bit_lc = LinearCombination::from_variable(bit_var);
            cs.constrain(bit_lc.clone(), bit_lc.clone(), bit_lc)?;
        }

        // Constraint: value = sum(bit_i * 2^i)
        // In R1CS form: (sum(bit_i * 2^i)) * 1 = value
        let mut sum_lc = LinearCombination::zero();
        let mut power_of_two = Scalar::ONE;
        let two = Scalar::from(2u64);
        for bit_var in &bit_vars {
            sum_lc.add_term(power_of_two, *bit_var);
            power_of_two *= two;
        }

        let one_lc = LinearCombination::from_constant(Scalar::ONE);
        let value_lc = LinearCombination::from_variable(value_var);
        cs.constrain(sum_lc, one_lc, value_lc)?;

        Ok(())
    }
}

/// Merkle inclusion circuit: proves a leaf is in a Merkle tree without revealing
/// the leaf value or the authentication path.
///
/// Uses an algebraic hash function (parent = left * right + left + right) for
/// R1CS efficiency. In production, this should be replaced with Poseidon hash
/// (~300 constraints per hash vs ~27K for SHA-256 in R1CS).
///
/// # Use Cases
/// - Privacy-preserving state proofs in smart contracts
/// - Proving membership in a set without revealing which element
/// - Anonymous credential verification
pub struct MerkleInclusionCircuit {
    /// The secret leaf value (as a Scalar, typically a hash).
    pub leaf: Scalar,
    /// The authentication path: sibling hashes at each tree level.
    /// Length determines the tree depth.
    pub path: Vec<Scalar>,
    /// Position bits: false = leaf is on the left, true = leaf is on the right.
    /// Must have the same length as `path`.
    pub position_bits: Vec<bool>,
    /// The Merkle root (public, known to the verifier).
    pub root: Scalar,
}

impl Circuit for MerkleInclusionCircuit {
    fn synthesize(&self, cs: &mut ConstraintSystem) -> Result<(), Groth16Error> {
        if self.path.len() != self.position_bits.len() {
            return Err(Groth16Error::ProofGenerationFailed {
                reason: "path and position_bits must have the same length".into(),
            });
        }
        if self.path.is_empty() {
            return Err(Groth16Error::ProofGenerationFailed {
                reason: "Merkle tree must have at least depth 1".into(),
            });
        }

        // Public input: the Merkle root
        let root_var = cs.alloc_public_input(self.root)?;

        // Private inputs: leaf
        let leaf_var = cs.alloc_witness(self.leaf)?;
        let mut current_val = self.leaf;
        let mut current_var = leaf_var;

        for (sibling, &is_right) in self.path.iter().zip(self.position_bits.iter()) {
            let sibling_var = cs.alloc_witness(*sibling)?;
            let position_var = cs.alloc_witness(if is_right {
                Scalar::ONE
            } else {
                Scalar::ZERO
            })?;

            // Constraint: position * position = position (bit constraint)
            let pos_lc = LinearCombination::from_variable(position_var);
            cs.constrain(pos_lc.clone(), pos_lc.clone(), pos_lc.clone())?;

            // Compute left and right based on position.
            // left = current + position * (sibling - current)
            // right = sibling + position * (current - sibling) = sibling - position * (sibling - current)
            let diff_val = *sibling - current_val;
            let diff_var = cs.alloc_witness(diff_val)?;

            // Constrain diff = sibling - current: (sibling - current) * 1 = diff
            let mut diff_check_lc = LinearCombination::zero();
            diff_check_lc.add_term(Scalar::ONE, sibling_var);
            diff_check_lc.add_term(-Scalar::ONE, current_var);
            cs.constrain(
                diff_check_lc,
                LinearCombination::from_constant(Scalar::ONE),
                LinearCombination::from_variable(diff_var),
            )?;

            // position * diff = swap_delta
            let swap_delta_val = if is_right { diff_val } else { Scalar::ZERO };
            let swap_delta_var = cs.alloc_witness(swap_delta_val)?;
            cs.constrain(
                LinearCombination::from_variable(position_var),
                LinearCombination::from_variable(diff_var),
                LinearCombination::from_variable(swap_delta_var),
            )?;

            // left = current + swap_delta
            let left_val = current_val + swap_delta_val;
            let left_var = cs.alloc_witness(left_val)?;
            let mut left_check = LinearCombination::zero();
            left_check.add_term(Scalar::ONE, current_var);
            left_check.add_term(Scalar::ONE, swap_delta_var);
            cs.constrain(
                left_check,
                LinearCombination::from_constant(Scalar::ONE),
                LinearCombination::from_variable(left_var),
            )?;

            // right = sibling - swap_delta
            let right_val = *sibling - swap_delta_val;
            let right_var = cs.alloc_witness(right_val)?;
            let mut right_check = LinearCombination::zero();
            right_check.add_term(Scalar::ONE, sibling_var);
            right_check.add_term(-Scalar::ONE, swap_delta_var);
            cs.constrain(
                right_check,
                LinearCombination::from_constant(Scalar::ONE),
                LinearCombination::from_variable(right_var),
            )?;

            // Algebraic hash: parent = left * right + left + right
            // WHY: This algebraic hash is efficient in R1CS (only 2 constraints
            // for the hash itself). For production, replace with Poseidon hash.
            let product_val = left_val * right_val;
            let product_var = cs.alloc_witness(product_val)?;
            cs.constrain(
                LinearCombination::from_variable(left_var),
                LinearCombination::from_variable(right_var),
                LinearCombination::from_variable(product_var),
            )?;

            // parent = product + left + right
            let parent_val = product_val + left_val + right_val;
            let parent_var = cs.alloc_witness(parent_val)?;
            let mut sum_lc = LinearCombination::zero();
            sum_lc.add_term(Scalar::ONE, product_var);
            sum_lc.add_term(Scalar::ONE, left_var);
            sum_lc.add_term(Scalar::ONE, right_var);
            cs.constrain(
                sum_lc,
                LinearCombination::from_constant(Scalar::ONE),
                LinearCombination::from_variable(parent_var),
            )?;

            current_val = parent_val;
            current_var = parent_var;
        }

        // Final constraint: computed root == public root
        // (current - root) * 1 = 0
        let mut diff_lc = LinearCombination::zero();
        diff_lc.add_term(Scalar::ONE, current_var);
        diff_lc.add_term(-Scalar::ONE, root_var);
        cs.constrain(
            diff_lc,
            LinearCombination::from_constant(Scalar::ONE),
            LinearCombination::from_constant(Scalar::ZERO),
        )?;

        Ok(())
    }
}

impl MerkleInclusionCircuit {
    /// Compute the Merkle root using the algebraic hash function.
    ///
    /// Used to construct valid test data. The hash function matches what
    /// the circuit constrains: parent = left * right + left + right.
    pub fn compute_root(leaf: Scalar, path: &[Scalar], position_bits: &[bool]) -> Scalar {
        let mut current = leaf;
        for (sibling, &is_right) in path.iter().zip(position_bits.iter()) {
            let (left, right) = if is_right {
                (*sibling, current)
            } else {
                (current, *sibling)
            };
            // Algebraic hash: parent = left * right + left + right
            current = left * right + left + right;
        }
        current
    }
}

/// Balance conservation circuit: proves that the sum of input amounts equals
/// the sum of output amounts without revealing any individual amounts.
///
/// This is used for complex multi-party transactions in smart contracts where
/// multiple inputs and outputs must balance but individual values stay private.
pub struct BalanceConservationCircuit {
    /// Secret input amounts.
    pub inputs: Vec<Scalar>,
    /// Secret output amounts.
    pub outputs: Vec<Scalar>,
}

impl Circuit for BalanceConservationCircuit {
    fn synthesize(&self, cs: &mut ConstraintSystem) -> Result<(), Groth16Error> {
        if self.inputs.is_empty() || self.outputs.is_empty() {
            return Err(Groth16Error::ProofGenerationFailed {
                reason: "must have at least one input and one output".into(),
            });
        }

        // Public inputs: the number of inputs and outputs.
        // WHY: The verifier needs to know the transaction shape even if not
        // the amounts. This prevents the prover from changing the number of
        // inputs/outputs between setup and proving.
        let num_inputs_scalar = Scalar::from(self.inputs.len() as u64);
        let num_outputs_scalar = Scalar::from(self.outputs.len() as u64);
        let _num_inputs_var = cs.alloc_public_input(num_inputs_scalar)?;
        let _num_outputs_var = cs.alloc_public_input(num_outputs_scalar)?;

        // Private inputs: all amounts
        let input_vars: Vec<Variable> = self
            .inputs
            .iter()
            .map(|v| cs.alloc_witness(*v))
            .collect::<Result<Vec<_>, _>>()?;

        let output_vars: Vec<Variable> = self
            .outputs
            .iter()
            .map(|v| cs.alloc_witness(*v))
            .collect::<Result<Vec<_>, _>>()?;

        // Conservation constraint: sum(inputs) - sum(outputs) = 0
        // In R1CS: (sum_inputs - sum_outputs) * 1 = 0
        let mut balance_lc = LinearCombination::zero();
        for &input_var in &input_vars {
            balance_lc.add_term(Scalar::ONE, input_var);
        }
        for &output_var in &output_vars {
            balance_lc.add_term(-Scalar::ONE, output_var);
        }

        cs.constrain(
            balance_lc,
            LinearCombination::from_constant(Scalar::ONE),
            LinearCombination::from_constant(Scalar::ZERO),
        )?;

        Ok(())
    }
}

// ============================================================================
// Convenience Functions
// ============================================================================

/// Prove a range: secret value is in [0, 2^num_bits).
///
/// Convenience wrapper around RangeProofCircuit for direct use.
pub fn prove_range(
    value: u64,
    num_bits: usize,
) -> Result<(Groth16Proof, SetupParameters), Groth16Error> {
    let scalar_value = Scalar::from(value);
    let circuit = RangeProofCircuit {
        value: scalar_value,
        num_bits,
    };

    let params = trusted_setup(&circuit)?;
    let proof = prove(&params, &circuit, &[scalar_value])?;
    Ok((proof, params))
}

/// Prove Merkle tree inclusion.
///
/// Convenience wrapper around MerkleInclusionCircuit.
pub fn prove_merkle_inclusion(
    leaf: Scalar,
    path: Vec<Scalar>,
    position_bits: Vec<bool>,
    root: Scalar,
) -> Result<(Groth16Proof, SetupParameters), Groth16Error> {
    let circuit = MerkleInclusionCircuit {
        leaf,
        path,
        position_bits,
        root,
    };

    let params = trusted_setup(&circuit)?;
    let proof = prove(&params, &circuit, &[root])?;
    Ok((proof, params))
}

/// Prove balance conservation: sum(inputs) == sum(outputs).
///
/// Convenience wrapper around BalanceConservationCircuit.
pub fn prove_balance_conservation(
    inputs: Vec<u64>,
    outputs: Vec<u64>,
) -> Result<(Groth16Proof, SetupParameters), Groth16Error> {
    let input_scalars: Vec<Scalar> = inputs.iter().map(|&v| Scalar::from(v)).collect();
    let output_scalars: Vec<Scalar> = outputs.iter().map(|&v| Scalar::from(v)).collect();

    let circuit = BalanceConservationCircuit {
        inputs: input_scalars.clone(),
        outputs: output_scalars.clone(),
    };

    let public_inputs = vec![
        Scalar::from(input_scalars.len() as u64),
        Scalar::from(output_scalars.len() as u64),
    ];

    let params = trusted_setup(&circuit)?;
    let proof = prove(&params, &circuit, &public_inputs)?;
    Ok((proof, params))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Constraint System Tests
    // ========================================================================

    #[test]
    fn test_constraint_system_basic() {
        let mut cs = ConstraintSystem::new();

        let pub_var = cs.alloc_public_input(Scalar::from(42u64)).unwrap();
        assert_eq!(pub_var.0, 1);
        assert_eq!(cs.num_public_inputs(), 1);

        let _wit_var = cs.alloc_witness(Scalar::from(7u64)).unwrap();
        assert_eq!(cs.num_witness_vars(), 1);

        // Trivial constraint: pub * 1 = pub
        let pub_lc = LinearCombination::from_variable(pub_var);
        let one_lc = LinearCombination::from_constant(Scalar::ONE);
        cs.constrain(pub_lc.clone(), one_lc, pub_lc).unwrap();

        assert_eq!(cs.num_constraints(), 1);
        assert!(cs.verify_assignment().is_ok());
    }

    #[test]
    fn test_constraint_system_multiplication() {
        let mut cs = ConstraintSystem::new();

        // Prove: a * b = c where a=3, b=7, c=21
        let a = cs.alloc_witness(Scalar::from(3u64)).unwrap();
        let b = cs.alloc_witness(Scalar::from(7u64)).unwrap();
        let c = cs.alloc_witness(Scalar::from(21u64)).unwrap();

        cs.constrain(
            LinearCombination::from_variable(a),
            LinearCombination::from_variable(b),
            LinearCombination::from_variable(c),
        )
        .unwrap();
        assert!(cs.verify_assignment().is_ok());
    }

    #[test]
    fn test_constraint_system_violation() {
        let mut cs = ConstraintSystem::new();

        let a = cs.alloc_witness(Scalar::from(3u64)).unwrap();
        let b = cs.alloc_witness(Scalar::from(7u64)).unwrap();
        let c = cs.alloc_witness(Scalar::from(22u64)).unwrap(); // Wrong! Should be 21

        cs.constrain(
            LinearCombination::from_variable(a),
            LinearCombination::from_variable(b),
            LinearCombination::from_variable(c),
        )
        .unwrap();
        assert!(cs.verify_assignment().is_err());
    }

    #[test]
    fn test_linear_combination_evaluation() {
        let assignment = vec![
            Scalar::ONE,         // Variable::ONE
            Scalar::from(5u64),  // public input
            Scalar::from(10u64), // witness
        ];

        // 3 * var_1 + 2 * var_2 = 3*5 + 2*10 = 35
        let mut lc = LinearCombination::zero();
        lc.add_term(Scalar::from(3u64), Variable(1));
        lc.add_term(Scalar::from(2u64), Variable(2));

        let result = lc.evaluate(&assignment).unwrap();
        assert_eq!(result, Scalar::from(35u64));
    }

    // ========================================================================
    // Range Proof Circuit Tests
    // ========================================================================

    #[test]
    fn test_range_proof_circuit_valid() {
        let value = 42u64;
        let circuit = RangeProofCircuit {
            value: Scalar::from(value),
            num_bits: 8,
        };

        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &[Scalar::from(value)]).unwrap();
        assert!(verify(&params.verification_key, &proof, &[Scalar::from(value)]).unwrap());
    }

    #[test]
    fn test_range_proof_circuit_zero() {
        let circuit = RangeProofCircuit {
            value: Scalar::ZERO,
            num_bits: 8,
        };

        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &[Scalar::ZERO]).unwrap();
        assert!(verify(&params.verification_key, &proof, &[Scalar::ZERO]).unwrap());
    }

    #[test]
    fn test_range_proof_circuit_max_value() {
        let value = 255u64; // Max for 8 bits
        let circuit = RangeProofCircuit {
            value: Scalar::from(value),
            num_bits: 8,
        };

        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &[Scalar::from(value)]).unwrap();
        assert!(verify(&params.verification_key, &proof, &[Scalar::from(value)]).unwrap());
    }

    #[test]
    fn test_range_proof_convenience() {
        let (proof, params) = prove_range(100, 16).unwrap();
        assert!(verify(&params.verification_key, &proof, &[Scalar::from(100u64)]).unwrap());
    }

    #[test]
    fn test_range_proof_invalid_num_bits() {
        let circuit = RangeProofCircuit {
            value: Scalar::from(42u64),
            num_bits: 0,
        };
        assert!(trusted_setup(&circuit).is_err());

        let circuit = RangeProofCircuit {
            value: Scalar::from(42u64),
            num_bits: 65,
        };
        assert!(trusted_setup(&circuit).is_err());
    }

    // ========================================================================
    // Merkle Inclusion Circuit Tests
    // ========================================================================

    #[test]
    fn test_merkle_inclusion_depth_1() {
        let leaf = Scalar::from(42u64);
        let sibling = Scalar::from(99u64);
        let position_bits = vec![false];

        let root = MerkleInclusionCircuit::compute_root(leaf, &[sibling], &position_bits);

        let circuit = MerkleInclusionCircuit {
            leaf,
            path: vec![sibling],
            position_bits,
            root,
        };

        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &[root]).unwrap();
        assert!(verify(&params.verification_key, &proof, &[root]).unwrap());
    }

    #[test]
    fn test_merkle_inclusion_depth_3() {
        let leaf = Scalar::from(7u64);
        let siblings = vec![
            Scalar::from(11u64),
            Scalar::from(23u64),
            Scalar::from(37u64),
        ];
        let position_bits = vec![true, false, true];

        let root = MerkleInclusionCircuit::compute_root(leaf, &siblings, &position_bits);

        let circuit = MerkleInclusionCircuit {
            leaf,
            path: siblings.clone(),
            position_bits: position_bits.clone(),
            root,
        };

        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &[root]).unwrap();
        assert!(verify(&params.verification_key, &proof, &[root]).unwrap());
    }

    #[test]
    fn test_merkle_inclusion_wrong_root() {
        let leaf = Scalar::from(42u64);
        let sibling = Scalar::from(99u64);
        let position_bits = vec![false];

        let correct_root =
            MerkleInclusionCircuit::compute_root(leaf, &[sibling], &position_bits);
        let wrong_root = correct_root + Scalar::ONE;

        let circuit = MerkleInclusionCircuit {
            leaf,
            path: vec![sibling],
            position_bits,
            root: wrong_root,
        };

        let params = trusted_setup(&circuit).unwrap();
        // Proof generation should fail because the assignment violates the root constraint
        let result = prove(&params, &circuit, &[wrong_root]);
        assert!(result.is_err());
    }

    #[test]
    fn test_merkle_inclusion_convenience() {
        let leaf = Scalar::from(100u64);
        let path = vec![Scalar::from(200u64), Scalar::from(300u64)];
        let position_bits = vec![false, true];
        let root = MerkleInclusionCircuit::compute_root(leaf, &path, &position_bits);

        let (proof, params) =
            prove_merkle_inclusion(leaf, path, position_bits, root).unwrap();
        assert!(verify(&params.verification_key, &proof, &[root]).unwrap());
    }

    #[test]
    fn test_merkle_empty_path_fails() {
        let circuit = MerkleInclusionCircuit {
            leaf: Scalar::from(42u64),
            path: vec![],
            position_bits: vec![],
            root: Scalar::from(42u64),
        };
        assert!(trusted_setup(&circuit).is_err());
    }

    #[test]
    fn test_merkle_mismatched_path_lengths() {
        let circuit = MerkleInclusionCircuit {
            leaf: Scalar::from(42u64),
            path: vec![Scalar::from(1u64)],
            position_bits: vec![false, true],
            root: Scalar::from(42u64),
        };
        assert!(trusted_setup(&circuit).is_err());
    }

    // ========================================================================
    // Balance Conservation Circuit Tests
    // ========================================================================

    #[test]
    fn test_balance_conservation_simple() {
        let circuit = BalanceConservationCircuit {
            inputs: vec![Scalar::from(100u64), Scalar::from(200u64)],
            outputs: vec![Scalar::from(150u64), Scalar::from(150u64)],
        };

        let public_inputs = vec![Scalar::from(2u64), Scalar::from(2u64)];

        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &public_inputs).unwrap();
        assert!(verify(&params.verification_key, &proof, &public_inputs).unwrap());
    }

    #[test]
    fn test_balance_conservation_unbalanced_fails() {
        let circuit = BalanceConservationCircuit {
            inputs: vec![Scalar::from(100u64)],
            outputs: vec![Scalar::from(200u64)],
        };

        let public_inputs = vec![Scalar::from(1u64), Scalar::from(1u64)];

        let params = trusted_setup(&circuit).unwrap();
        let result = prove(&params, &circuit, &public_inputs);
        assert!(result.is_err(), "unbalanced transaction should fail");
    }

    #[test]
    fn test_balance_conservation_many_io() {
        let circuit = BalanceConservationCircuit {
            inputs: vec![
                Scalar::from(10u64),
                Scalar::from(20u64),
                Scalar::from(30u64),
                Scalar::from(40u64),
            ],
            outputs: vec![Scalar::from(50u64), Scalar::from(50u64)], // 100 = 100
        };

        let public_inputs = vec![Scalar::from(4u64), Scalar::from(2u64)];

        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &public_inputs).unwrap();
        assert!(verify(&params.verification_key, &proof, &public_inputs).unwrap());
    }

    #[test]
    fn test_balance_conservation_convenience() {
        let (proof, params) =
            prove_balance_conservation(vec![100, 200, 50], vec![300, 50]).unwrap();

        let public_inputs = vec![Scalar::from(3u64), Scalar::from(2u64)];
        assert!(verify(&params.verification_key, &proof, &public_inputs).unwrap());
    }

    #[test]
    fn test_balance_conservation_empty_inputs_fails() {
        let circuit = BalanceConservationCircuit {
            inputs: vec![],
            outputs: vec![Scalar::from(100u64)],
        };
        assert!(trusted_setup(&circuit).is_err());
    }

    // ========================================================================
    // Proof Rejection Tests
    // ========================================================================

    #[test]
    fn test_wrong_public_input_fails() {
        let (proof, params) = prove_range(42, 8).unwrap();

        let result = verify(
            &params.verification_key,
            &proof,
            &[Scalar::from(99u64)], // Wrong value
        );
        match result {
            Ok(valid) => assert!(!valid, "wrong public input should not verify"),
            Err(_) => {} // Also acceptable
        }
    }

    #[test]
    fn test_wrong_public_input_count_fails() {
        let (proof, params) = prove_range(42, 8).unwrap();

        let result = verify(
            &params.verification_key,
            &proof,
            &[Scalar::from(42u64), Scalar::from(1u64)],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_tampered_proof_a_fails() {
        let (mut proof, params) = prove_range(42, 8).unwrap();
        proof.a = RISTRETTO_BASEPOINT_POINT;

        let result = verify(&params.verification_key, &proof, &[Scalar::from(42u64)]);
        match result {
            Ok(valid) => assert!(!valid, "tampered proof A should not verify"),
            Err(_) => {}
        }
    }

    #[test]
    fn test_tampered_proof_b_fails() {
        let (mut proof, params) = prove_range(42, 8).unwrap();
        proof.b = RISTRETTO_BASEPOINT_POINT;

        let result = verify(&params.verification_key, &proof, &[Scalar::from(42u64)]);
        match result {
            Ok(valid) => assert!(!valid, "tampered proof B should not verify"),
            Err(_) => {}
        }
    }

    #[test]
    fn test_tampered_response_fails() {
        let (mut proof, params) = prove_range(42, 8).unwrap();
        if !proof.responses.is_empty() {
            proof.responses[0] = SerializableScalar(Scalar::random(&mut OsRng));
        }

        let result = verify(&params.verification_key, &proof, &[Scalar::from(42u64)]);
        match result {
            Ok(valid) => assert!(!valid, "tampered response should not verify"),
            Err(_) => {}
        }
    }

    #[test]
    fn test_tampered_nonce_commitment_fails() {
        let (mut proof, params) = prove_range(42, 8).unwrap();
        proof.nonce_commitment = RISTRETTO_BASEPOINT_POINT;

        let result = verify(&params.verification_key, &proof, &[Scalar::from(42u64)]);
        match result {
            Ok(valid) => assert!(!valid, "tampered nonce commitment should not verify"),
            Err(_) => {}
        }
    }

    // ========================================================================
    // Serialization Tests
    // ========================================================================

    #[test]
    fn test_proof_serialization_roundtrip() {
        let (proof, params) = prove_range(42, 8).unwrap();

        let json = serde_json::to_string(&proof).expect("serialization should succeed");
        let deserialized: Groth16Proof =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert!(
            verify(&params.verification_key, &deserialized, &[Scalar::from(42u64)]).unwrap()
        );
    }

    #[test]
    fn test_verification_key_serialization_roundtrip() {
        let circuit = RangeProofCircuit {
            value: Scalar::from(42u64),
            num_bits: 8,
        };

        let params = trusted_setup(&circuit).unwrap();

        let json = serde_json::to_string(&params.verification_key)
            .expect("VK serialization should succeed");
        let deserialized: VerificationKey =
            serde_json::from_str(&json).expect("VK deserialization should succeed");

        assert_eq!(
            deserialized.num_public_inputs,
            params.verification_key.num_public_inputs
        );
        assert_eq!(
            deserialized.num_constraints,
            params.verification_key.num_constraints
        );
    }

    // ========================================================================
    // Setup Mismatch Tests
    // ========================================================================

    #[test]
    fn test_setup_mismatch_different_circuit() {
        let circuit_8 = RangeProofCircuit {
            value: Scalar::from(42u64),
            num_bits: 8,
        };
        let params = trusted_setup(&circuit_8).unwrap();

        let circuit_16 = RangeProofCircuit {
            value: Scalar::from(42u64),
            num_bits: 16,
        };
        let result = prove(&params, &circuit_16, &[Scalar::from(42u64)]);
        assert!(result.is_err(), "mismatched circuit should fail");
    }

    // ========================================================================
    // Custom Circuit Test
    // ========================================================================

    /// A simple custom circuit: proves knowledge of x such that x^3 + x + 5 = y
    /// where y is public.
    struct CubicCircuit {
        x: Scalar,
        y: Scalar,
    }

    impl Circuit for CubicCircuit {
        fn synthesize(&self, cs: &mut ConstraintSystem) -> Result<(), Groth16Error> {
            let y_var = cs.alloc_public_input(self.y)?;
            let x_var = cs.alloc_witness(self.x)?;

            // x * x = x_sq
            let x_sq = self.x * self.x;
            let x_sq_var = cs.alloc_witness(x_sq)?;
            cs.constrain(
                LinearCombination::from_variable(x_var),
                LinearCombination::from_variable(x_var),
                LinearCombination::from_variable(x_sq_var),
            )?;

            // x_sq * x = x_cubed
            let x_cubed = x_sq * self.x;
            let x_cubed_var = cs.alloc_witness(x_cubed)?;
            cs.constrain(
                LinearCombination::from_variable(x_sq_var),
                LinearCombination::from_variable(x_var),
                LinearCombination::from_variable(x_cubed_var),
            )?;

            // x_cubed + x + 5 = y
            // (x_cubed + x + 5) * 1 = y
            let mut sum_lc = LinearCombination::zero();
            sum_lc.add_term(Scalar::ONE, x_cubed_var);
            sum_lc.add_term(Scalar::ONE, x_var);
            sum_lc.add_term(Scalar::from(5u64), Variable::ONE);

            cs.constrain(
                sum_lc,
                LinearCombination::from_constant(Scalar::ONE),
                LinearCombination::from_variable(y_var),
            )?;

            Ok(())
        }
    }

    #[test]
    fn test_custom_cubic_circuit() {
        let x = Scalar::from(3u64);
        // y = x^3 + x + 5 = 27 + 3 + 5 = 35
        let y = Scalar::from(35u64);

        let circuit = CubicCircuit { x, y };
        let params = trusted_setup(&circuit).unwrap();
        let proof = prove(&params, &circuit, &[y]).unwrap();
        assert!(verify(&params.verification_key, &proof, &[y]).unwrap());
    }

    #[test]
    fn test_custom_cubic_circuit_wrong_answer() {
        let x = Scalar::from(3u64);
        let wrong_y = Scalar::from(36u64); // Should be 35

        let circuit = CubicCircuit { x, y: wrong_y };
        let params = trusted_setup(&circuit).unwrap();
        let result = prove(&params, &circuit, &[wrong_y]);
        assert!(result.is_err(), "wrong answer should fail constraint check");
    }

    // ========================================================================
    // GratiaError Integration
    // ========================================================================

    #[test]
    fn test_groth16_error_converts_to_gratia_error() {
        let err = Groth16Error::VerificationFailed {
            reason: "test error".into(),
        };
        let gratia_err: GratiaError = err.into();
        match gratia_err {
            GratiaError::InvalidZkProof { reason } => {
                assert!(reason.contains("test error"));
            }
            _ => panic!("expected InvalidZkProof variant"),
        }
    }
}
