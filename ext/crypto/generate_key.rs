// Copyright 2018-2026 the Deno authors. MIT license.

use aws_lc_rs::rand::SecureRandom;
use aws_lc_rs::signature::EcdsaKeyPair;
use elliptic_curve::Generate;
use elliptic_curve::pkcs8::EncodePrivateKey;
use rand::rand_core::UnwrapErr;
use rand::rngs::SysRng;
use rsa::BoxedUint;
use rsa::PrivateKey as RsaPrivateKey;
use rsa::pkcs1::EncodeRsaPrivateKey;

use crate::shared::*;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class("DOMExceptionOperationError")]
pub enum GenerateKeyError {
  #[class(inherit)]
  #[error(transparent)]
  General(
    #[from]
    #[inherit]
    SharedError,
  ),
  #[error("Bad public exponent")]
  BadPublicExponent,
  #[error("Invalid HMAC key length")]
  InvalidHMACKeyLength,
  #[error("Failed to serialize RSA key")]
  FailedRSAKeySerialization,
  #[error("Invalid AES key length")]
  InvalidAESKeyLength,
  #[error("Failed to generate RSA key")]
  FailedRSAKeyGeneration,
  #[error("Failed to generate EC key")]
  FailedECKeyGeneration,
  #[error("Failed to generate key")]
  FailedKeyGeneration,
  #[error("Unsupported algorithm")]
  UnsupportedAlgorithm,
}

// Allowlist for RSA public exponents.
const PUB_EXPONENT_1: u64 = 3;
const PUB_EXPONENT_2: u64 = 65537;

fn public_exponent_to_u64(bytes: &[u8]) -> Option<u64> {
  let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
  let bytes = &bytes[start..];
  if bytes.len() > 8 {
    return None;
  }
  Some(bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

/// Rust-callable view of the RSA keygen path. Returns raw PKCS#1 DER
/// bytes ready to feed [`crate::shared::RawKeyData::Private`].
pub fn generate_rsa(
  modulus_length: u32,
  public_exponent: &[u8],
) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_rsa(modulus_length, public_exponent)
}

/// Rust-callable EC keygen.
pub fn generate_ec(
  named_curve: EcNamedCurve,
) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_ec(named_curve)
}

/// Rust-callable AES keygen.
pub fn generate_aes(length: usize) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_aes(length)
}

/// Rust-callable HMAC keygen.
pub fn generate_hmac(
  hash: ShaHash,
  length: Option<usize>,
) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_hmac(hash, length)
}

fn generate_key_rsa(
  modulus_length: u32,
  public_exponent: &[u8],
) -> Result<Vec<u8>, GenerateKeyError> {
  let exponent = public_exponent_to_u64(public_exponent)
    .filter(|e| *e == PUB_EXPONENT_1 || *e == PUB_EXPONENT_2)
    .ok_or(GenerateKeyError::BadPublicExponent)?;

  let mut rng = UnwrapErr(SysRng);

  let private_key = RsaPrivateKey::new_with_exp(
    &mut rng,
    modulus_length as usize,
    BoxedUint::from(exponent),
  )
  .map_err(|_| GenerateKeyError::FailedRSAKeyGeneration)?;

  let private_key = private_key
    .to_pkcs1_der()
    .map_err(|_| GenerateKeyError::FailedRSAKeySerialization)?;

  Ok(private_key.as_bytes().to_vec())
}

fn generate_key_ec_p521() -> Result<Vec<u8>, GenerateKeyError> {
  let key = p521::SecretKey::generate();
  let pkcs8 = key
    .to_pkcs8_der()
    .map_err(|_| GenerateKeyError::FailedECKeyGeneration)?;
  Ok(pkcs8.as_bytes().to_vec())
}

fn generate_key_ec(
  named_curve: EcNamedCurve,
) -> Result<Vec<u8>, GenerateKeyError> {
  let curve = match named_curve {
    EcNamedCurve::P256 => {
      &aws_lc_rs::signature::ECDSA_P256_SHA256_FIXED_SIGNING
    }
    EcNamedCurve::P384 => {
      &aws_lc_rs::signature::ECDSA_P384_SHA384_FIXED_SIGNING
    }
    EcNamedCurve::P521 => return generate_key_ec_p521(),
  };

  let rng = aws_lc_rs::rand::SystemRandom::new();

  let pkcs8 = EcdsaKeyPair::generate_pkcs8(curve, &rng)
    .map_err(|_| GenerateKeyError::FailedECKeyGeneration)?;

  Ok(pkcs8.as_ref().to_vec())
}

fn generate_key_aes(length: usize) -> Result<Vec<u8>, GenerateKeyError> {
  if !length.is_multiple_of(8) || length > 256 {
    return Err(GenerateKeyError::InvalidAESKeyLength);
  }

  let mut key = vec![0u8; length / 8];
  let rng = aws_lc_rs::rand::SystemRandom::new();
  rng
    .fill(&mut key)
    .map_err(|_| GenerateKeyError::FailedKeyGeneration)?;

  Ok(key)
}

fn generate_key_hmac(
  hash: ShaHash,
  length: Option<usize>,
) -> Result<Vec<u8>, GenerateKeyError> {
  // Default key length (in bytes) is the hash's block size.
  // SHA-3 is not supported by aws-lc-rs for HMAC, so the block sizes are
  // hard-coded here per FIPS 202.
  let default_block_len = match hash {
    ShaHash::Sha1 => aws_lc_rs::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY
      .digest_algorithm()
      .block_len(),
    ShaHash::Sha256 => {
      aws_lc_rs::hmac::HMAC_SHA256.digest_algorithm().block_len()
    }
    ShaHash::Sha384 => {
      aws_lc_rs::hmac::HMAC_SHA384.digest_algorithm().block_len()
    }
    ShaHash::Sha512 => {
      aws_lc_rs::hmac::HMAC_SHA512.digest_algorithm().block_len()
    }
    // FIPS 202: rate (r) in bytes for SHA3-N is (1600 - 2N) / 8.
    ShaHash::Sha3_256 => 136,
    ShaHash::Sha3_384 => 104,
    ShaHash::Sha3_512 => 72,
  };

  let length = if let Some(length) = length {
    if length % 8 != 0 {
      return Err(GenerateKeyError::InvalidHMACKeyLength);
    }

    let length = length / 8;
    if length > aws_lc_rs::digest::MAX_BLOCK_LEN {
      return Err(GenerateKeyError::InvalidHMACKeyLength);
    }

    length
  } else {
    default_block_len
  };

  let rng = aws_lc_rs::rand::SystemRandom::new();
  let mut key = vec![0u8; length];
  rng
    .fill(&mut key)
    .map_err(|_| GenerateKeyError::FailedKeyGeneration)?;

  Ok(key)
}
