use anyhow::{anyhow, Error, Result};
use chrono::{DateTime, Utc};
use ecdsa::signature::Verifier;
use ecdsa::{Signature as OtherSignature, VerifyingKey};
use p256::pkcs8::FromPublicKey;
use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};
use serde_plain::{derive_display_from_serialize, derive_fromstr_from_deserialize};
use std::{collections::HashMap, convert::TryFrom, num::NonZeroU64};
use x509_parser::{parse_x509_certificate, pem::parse_x509_pem};

pub type CosignVerificationKey = VerifyingKey<p256::NistP256>;

// A signed root policy object
#[derive(Serialize, Deserialize)]
pub struct Policy {
    // A list of signatures.
    pub signatures: Vec<Signature>,
    // The root policy that is signed.
    pub signed: Signed,
}

impl Policy {
    pub fn validate_expires(&self) -> chrono::Duration {
        self.signed.expires.signed_duration_since(Utc::now())
    }

    /// Extract the public key from the policy
    pub fn extract_pub_key(&self) -> Result<CosignVerificationKey, anyhow::Error> {
        let cert = base64::decode(&self.signatures[0].cert)?;
        let (_, pem) = parse_x509_pem(&cert)
            .map_err(|e| anyhow!("Error parsing fulcio PEM certificate: {:?}", e))?;
        let (_, res_x509) = parse_x509_certificate(&pem.contents)
            .map_err(|e| anyhow!("Error parsing fulcio certificate: {:?}", e))?;
        let pub_key_bytes = res_x509.public_key().raw.to_owned();
        VerifyingKey::<p256::NistP256>::from_public_key_der(&pub_key_bytes[..])
            .map_err(|e| anyhow!("Cannot load key: {:?}", e))
    }

    /// Verify the signature provided has been actually generated by the given key against the
    /// when signing the provided message.
    pub fn verify_signature(
        &self,
        verification_key: &CosignVerificationKey,
        msg: &[u8],
    ) -> Result<()> {
        let signature_raw = base64::decode(&self.signatures[0].sig)?;
        let signature = OtherSignature::<p256::NistP256>::from_der(&signature_raw)?;
        verification_key
            .verify(msg, &signature)
            .map_err(|e| anyhow!("Verification failed: {:?}", e))
    }
}

// This holds the raw data from a serialized policy, accessible via the
// 'signatures' and 'signed' fields. We must preserve this data as RawValues
// in order for signature verification to work.
#[derive(Serialize, Deserialize)]
struct RawPolicy<'a> {
    #[serde(borrow)]
    pub signatures: &'a RawValue,
    #[serde(borrow)]
    pub signed: &'a RawValue,
}

// A signature and the key ID and certificate that made it.
#[derive(Serialize, Deserialize)]
pub struct Signature {
    // The hex encoded key ID that made this signature.
    pub keyid: String,
    // The base64 encoded signature of the canonical JSON of the root policy.
    pub sig: String,
    // The base64 encoded certificate that was used to create the signature.
    pub cert: String,
}

// The root policy indicated the trusted root keys.
#[derive(Serialize, Deserialize)]
pub struct Signed {
    pub consistent_snapshot: bool,
    pub expires: DateTime<Utc>,
    pub keys: HashMap<String, Key>,
    pub namespace: String,
    pub roles: HashMap<String, RoleKeys>,
    pub spec_version: String,
    pub version: NonZeroU64,
}

#[derive(Serialize, Deserialize)]
pub struct RoleKeys {
    /// The key IDs used for the role.
    pub keyids: Vec<String>,
    /// The threshold of signatures required to validate the role.
    pub threshold: NonZeroU64,
}

#[derive(PartialEq, Eq, Hash, Serialize, Deserialize)]
/// The type of metadata role.
pub enum RoleType {
    /// The root role delegates trust to specific keys trusted for all other top-level roles used in
    /// the system.
    Root,
}

impl TryFrom<&str> for RoleType {
    type Error = Error;
    fn try_from(s: &str) -> Result<Self> {
        match s {
            "Root" => Ok(RoleType::Root),
            other => Err(anyhow!("Unknown RoleType: {}", other)),
        }
    }
}

derive_display_from_serialize!(RoleType);
derive_fromstr_from_deserialize!(RoleType);

#[derive(Serialize, Deserialize)]
#[serde(tag = "keytype")]
pub enum Key {
    /// A sigstore oidc key.
    #[serde(rename = "sigstore-oidc")]
    SigstoreOidc {
        /// The sigstore oidc key.
        keyval: SigstoreOidcKey,
        /// Denotes the key's scheme
        scheme: String,
        /// Any additional fields read during deserialization; will not be used.
        // TODO: key_hash_algorithms
        #[serde(flatten)]
        _extra: HashMap<String, Value>,
    },
}

derive_display_from_serialize!(Key);
derive_fromstr_from_deserialize!(Key);

#[derive(Serialize, Deserialize)]
/// Represents a deserialized (decoded) SigstoreOidc public key.
pub struct SigstoreOidcKey {
    /// The identity (subject)
    pub identity: String,
    /// The issuer
    pub issuer: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::read,
        path::{Path, PathBuf},
    };

    const CRATE: &str = env!("CARGO_MANIFEST_DIR");

    struct Setup {
        good_policy: PathBuf,
        bad_policy: PathBuf,
    }

    impl Setup {
        fn new() -> Self {
            let good_policy = Path::new(CRATE).join("tests/test_data/policy_good.json");
            let bad_policy = Path::new(CRATE).join("tests/test_data/policy_bad.json");

            Self {
                good_policy,
                bad_policy,
            }
        }

        fn read_good_policy(&self) -> Policy {
            let raw_json = read(&self.good_policy).expect("Cannot read good policy file");
            serde_json::from_slice(&raw_json).expect("Cannot deserialize policy")
        }

        fn read_bad_policy(&self) -> Policy {
            let raw_json = read(&self.bad_policy).expect("Cannot read bad policy file");
            serde_json::from_slice(&raw_json).expect("Cannot deserialize policy")
        }
    }

    #[test]
    fn deserialize() {
        let setup = Setup::new();
        setup.read_good_policy();
    }

    #[test]
    fn parse_script_success() {
        let setup = Setup::new();
        let policy = setup.read_good_policy();
        assert_eq!(policy.signed.version, NonZeroU64::new(1).unwrap()) //#[allow_ci]
    }

    #[test]
    fn validate_expiry_success() {
        let setup = Setup::new();
        let policy = setup.read_good_policy();
        assert!(!policy.validate_expires().to_std().is_err());
    }

    #[test]
    fn validate_expiry_failure() {
        let setup = Setup::new();
        let policy = setup.read_bad_policy();
        assert!(policy.validate_expires().to_std().is_err());
    }

    // Note: open an issue about getting tests to run on Windows
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn validate_signature_success() {
        let setup = Setup::new();
        let policy = setup.read_good_policy();

        let good_policy = Path::new(CRATE).join("tests/test_data/policy_good.json");
        let raw_json = read(good_policy).expect("Cannot read good policy file");
        let raw_policy: RawPolicy =
            serde_json::from_slice(&raw_json).expect("Could not create Raw Policy");

        let pub_key = policy.extract_pub_key();
        let msg = (raw_policy.signed).get().as_bytes();

        let outcome = policy.verify_signature(&pub_key.unwrap(), msg); //#[allow_ci]
        assert!(outcome.is_ok());
    }

    #[test]
    fn validate_signature_failure() {
        let setup = Setup::new();
        let policy = setup.read_bad_policy();

        let bad_policy = Path::new(CRATE).join("tests/test_data/policy_bad.json");
        let raw_json = read(bad_policy).expect("Cannot read bad policy file");
        let raw_policy: RawPolicy =
            serde_json::from_slice(&raw_json).expect("Could not create Raw Policy");

        let pub_key = policy.extract_pub_key();
        let msg = (raw_policy.signed).get().as_bytes();

        let outcome = policy.verify_signature(&pub_key.unwrap(), msg); //#[allow_ci]
        assert!(outcome.is_err());
    }
}