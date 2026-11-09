// Copyright 2018-2026 the Deno authors. MIT license.

use aes_gcm::AeadInOut;
use aes_gcm::KeyInit;
use aes_gcm::Nonce;
use aes_gcm::aead::array::ArraySize;
use aes_gcm::aead::consts::U12;
use aes_gcm::aead::consts::U16;
use aes_gcm::aes::Aes128;
use aes_gcm::aes::Aes192;
use aes_gcm::aes::Aes256;
use aws_lc_rs::aead::Aad;
use aws_lc_rs::aead::CHACHA20_POLY1305;
use aws_lc_rs::aead::LessSafeKey;
use aws_lc_rs::aead::Nonce as AwsNonce;
use aws_lc_rs::aead::UnboundKey;
use cbc::cipher::BlockModeEncrypt;
use cbc::cipher::KeyIvInit;
use cbc::cipher::StreamCipher;
use cbc::cipher::block_padding::Pkcs7;
use ctr::Ctr32BE;
use ctr::Ctr64BE;
use ctr::Ctr128BE;
use rand::rand_core::UnwrapErr;
use rand::rngs::SysRng;
use rsa::pkcs1::DecodeRsaPublicKey;

use crate::shared::*;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum EncryptError {
  #[class(inherit)]
  #[error(transparent)]
  General(
    #[from]
    #[inherit]
    SharedError,
  ),
  #[class(type)]
  #[error("invalid length")]
  InvalidLength,
  #[class("DOMExceptionOperationError")]
  #[error("invalid key or iv")]
  InvalidKeyOrIv,
  #[class(type)]
  #[error("iv length not equal to 12 or 16")]
  InvalidIvLength,
  #[class("DOMExceptionOperationError")]
  #[error("invalid tag length")]
  InvalidTagLength,
  #[class(type)]
  #[error("invalid ChaCha20-Poly1305 nonce length: expected 12 bytes")]
  InvalidChaChaNonceLength,
  #[class(type)]
  #[error("invalid ChaCha20-Poly1305 key length: expected 32 bytes")]
  InvalidChaChaKeyLength,
  #[class(type)]
  #[error("invalid counter length. Currently supported 32/64/128 bits")]
  InvalidCounterLength,
  #[class("DOMExceptionOperationError")]
  #[error("tried to encrypt too much data")]
  TooMuchData,
  #[class("DOMExceptionOperationError")]
  #[error("Encryption failed")]
  Failed,
}

pub(crate) fn encrypt_rsa_oaep(
  key: &RawKeyData,
  hash: ShaHash,
  label: Vec<u8>,
  data: &[u8],
) -> Result<Vec<u8>, EncryptError> {
  let public_key = key.as_rsa_public_key()?;
  let public_key = rsa::PublicKey::from_pkcs1_der(&public_key)
    .map_err(|_| SharedError::FailedDecodePublicKey)?;
  let mut rng = UnwrapErr(SysRng);
  let encrypted = rsa_oaep_encrypt(&public_key, &mut rng, hash, &label, data)
    .map_err(|_| EncryptError::Failed)?;
  Ok(encrypted)
}

pub(crate) fn encrypt_aes_cbc(
  key: &RawKeyData,
  length: usize,
  iv: Vec<u8>,
  data: &[u8],
) -> Result<Vec<u8>, EncryptError> {
  let key = key.as_secret_key()?;
  let ciphertext = match length {
    128 => {
      // Section 10.3 Step 2 of RFC 2315 https://www.rfc-editor.org/rfc/rfc2315
      type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

      let cipher = Aes128CbcEnc::new_from_slices(key, &iv)
        .map_err(|_| EncryptError::InvalidKeyOrIv)?;
      cipher.encrypt_padded_vec::<Pkcs7>(data)
    }
    192 => {
      // Section 10.3 Step 2 of RFC 2315 https://www.rfc-editor.org/rfc/rfc2315
      type Aes192CbcEnc = cbc::Encryptor<aes::Aes192>;

      let cipher = Aes192CbcEnc::new_from_slices(key, &iv)
        .map_err(|_| EncryptError::InvalidKeyOrIv)?;
      cipher.encrypt_padded_vec::<Pkcs7>(data)
    }
    256 => {
      // Section 10.3 Step 2 of RFC 2315 https://www.rfc-editor.org/rfc/rfc2315
      type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

      let cipher = Aes256CbcEnc::new_from_slices(key, &iv)
        .map_err(|_| EncryptError::InvalidKeyOrIv)?;
      cipher.encrypt_padded_vec::<Pkcs7>(data)
    }
    _ => return Err(EncryptError::InvalidLength),
  };
  Ok(ciphertext)
}

fn encrypt_aes_gcm_general<N: ArraySize>(
  key: &[u8],
  iv: Vec<u8>,
  length: usize,
  ciphertext: &mut [u8],
  additional_data: Vec<u8>,
) -> Result<aes_gcm::Tag, EncryptError> {
  let nonce: &Nonce<N> = iv
    .as_slice()
    .try_into()
    .map_err(|_| EncryptError::InvalidIvLength)?;
  let tag = match length {
    128 => {
      let cipher = aes_gcm::AesGcm::<Aes128, N>::new_from_slice(key)
        .map_err(|_| EncryptError::Failed)?;
      cipher
        .encrypt_inout_detached(nonce, &additional_data, ciphertext.into())
        .map_err(|_| EncryptError::Failed)?
    }
    192 => {
      let cipher = aes_gcm::AesGcm::<Aes192, N>::new_from_slice(key)
        .map_err(|_| EncryptError::Failed)?;
      cipher
        .encrypt_inout_detached(nonce, &additional_data, ciphertext.into())
        .map_err(|_| EncryptError::Failed)?
    }
    256 => {
      let cipher = aes_gcm::AesGcm::<Aes256, N>::new_from_slice(key)
        .map_err(|_| EncryptError::Failed)?;
      cipher
        .encrypt_inout_detached(nonce, &additional_data, ciphertext.into())
        .map_err(|_| EncryptError::Failed)?
    }
    _ => return Err(EncryptError::InvalidLength),
  };

  Ok(tag)
}

pub(crate) fn encrypt_aes_gcm(
  key: &RawKeyData,
  length: usize,
  tag_length: usize,
  iv: Vec<u8>,
  additional_data: Option<Vec<u8>>,
  data: &[u8],
) -> Result<Vec<u8>, EncryptError> {
  let key = key.as_secret_key()?;
  let additional_data = additional_data.unwrap_or_default();

  let mut ciphertext = data.to_vec();
  // Fixed 96-bit OR 128-bit nonce
  let tag = match iv.len() {
    12 => encrypt_aes_gcm_general::<U12>(
      key,
      iv,
      length,
      &mut ciphertext,
      additional_data,
    )?,
    16 => encrypt_aes_gcm_general::<U16>(
      key,
      iv,
      length,
      &mut ciphertext,
      additional_data,
    )?,
    _ => return Err(EncryptError::InvalidIvLength),
  };

  // Truncated tag to the specified tag length.
  // `tag` is fixed to be 16 bytes long and (tag_length / 8) is always <= 16
  let tag = &tag[..(tag_length / 8)];

  // C | T
  ciphertext.extend_from_slice(tag);

  Ok(ciphertext)
}

pub(crate) fn encrypt_aes_ocb(
  key: &RawKeyData,
  length: usize,
  tag_length: usize,
  iv: Vec<u8>,
  additional_data: Option<Vec<u8>>,
  data: &[u8],
) -> Result<Vec<u8>, EncryptError> {
  use ocb3::Ocb3;
  use ocb3::aead::AeadInOut as Ocb3AeadInOut;
  use ocb3::aead::KeyInit as Ocb3KeyInit;
  use ocb3::aead::array::Array;
  use ocb3::aead::consts::U6;
  use ocb3::aead::consts::U7;
  use ocb3::aead::consts::U8;
  use ocb3::aead::consts::U9;
  use ocb3::aead::consts::U10;
  use ocb3::aead::consts::U11;
  use ocb3::aead::consts::U12;
  use ocb3::aead::consts::U13;
  use ocb3::aead::consts::U14;
  use ocb3::aead::consts::U15;
  use ocb3::aead::consts::U16;

  let key = key.as_secret_key()?;
  let additional_data = additional_data.unwrap_or_default();

  let mut ciphertext = data.to_vec();

  // The WICG spec permits a 64-, 96- or 128-bit tag for AES-OCB. The `ocb3`
  // crate's TagSize is a compile-time type parameter, so map each permitted
  // length to its typenum (U8/U12/U16). Anything else is an OperationError.
  let tag_size = match tag_length {
    64 => OcbTagSize::U8,
    96 => OcbTagSize::U12,
    128 => OcbTagSize::U16,
    _ => return Err(EncryptError::InvalidTagLength),
  };

  // RFC 7253 permits nonces up to 15 bytes; the `ocb3` crate supports 6..=15.
  // The NonceSize is a compile-time type parameter, so dispatch the runtime
  // length to the matching typenum. Lengths outside 6..=15 are unsupported.
  if iv.len() < 6 || iv.len() > 15 {
    return Err(EncryptError::InvalidIvLength);
  }

  // For a concrete (cipher, nonce, tag) triple the `ocb3` sealed NonceSizes /
  // TagSizes bounds are satisfied automatically; encrypt in place and return
  // the detached tag (already exactly `$tag` bytes long, no truncation).
  macro_rules! ocb_encrypt {
    ($aes:ty, $nonce:ty, $tag:ty) => {{
      let cipher = Ocb3::<$aes, $nonce, $tag>::new_from_slice(key)
        .map_err(|_| EncryptError::Failed)?;
      let nonce: &Array<u8, $nonce> = iv
        .as_slice()
        .try_into()
        .map_err(|_| EncryptError::InvalidIvLength)?;
      cipher
        .encrypt_inout_detached(
          nonce,
          &additional_data,
          ciphertext.as_mut_slice().into(),
        )
        .map_err(|_| EncryptError::Failed)?
        .to_vec()
    }};
  }
  macro_rules! ocb_encrypt_for_tag {
    ($nonce:ty, $tag:ty) => {
      match length {
        128 => ocb_encrypt!(aes::Aes128, $nonce, $tag),
        192 => ocb_encrypt!(aes::Aes192, $nonce, $tag),
        256 => ocb_encrypt!(aes::Aes256, $nonce, $tag),
        _ => return Err(EncryptError::InvalidLength),
      }
    };
  }
  macro_rules! ocb_encrypt_for_key {
    ($nonce:ty) => {
      match tag_size {
        OcbTagSize::U8 => ocb_encrypt_for_tag!($nonce, U8),
        OcbTagSize::U12 => ocb_encrypt_for_tag!($nonce, U12),
        OcbTagSize::U16 => ocb_encrypt_for_tag!($nonce, U16),
      }
    };
  }

  let tag = match iv.len() {
    6 => ocb_encrypt_for_key!(U6),
    7 => ocb_encrypt_for_key!(U7),
    8 => ocb_encrypt_for_key!(U8),
    9 => ocb_encrypt_for_key!(U9),
    10 => ocb_encrypt_for_key!(U10),
    11 => ocb_encrypt_for_key!(U11),
    12 => ocb_encrypt_for_key!(U12),
    13 => ocb_encrypt_for_key!(U13),
    14 => ocb_encrypt_for_key!(U14),
    15 => ocb_encrypt_for_key!(U15),
    _ => return Err(EncryptError::InvalidIvLength),
  };

  // The detached tag is already `tag_length / 8` bytes (TagSize), so no
  // truncation is required.
  // C | T
  ciphertext.extend_from_slice(&tag);

  Ok(ciphertext)
}

/// The AES-OCB tag sizes permitted by the WICG WebCrypto extension, mapping
/// each WICG `tagLength` to the `ocb3` TagSize typenum used at instantiation.
#[derive(Clone, Copy)]
enum OcbTagSize {
  U8,
  U12,
  U16,
}

pub(crate) fn encrypt_chacha20_poly1305(
  key: &RawKeyData,
  nonce: &[u8],
  additional_data: Option<Vec<u8>>,
  data: &[u8],
) -> Result<Vec<u8>, EncryptError> {
  let key_bytes = key.as_secret_key()?;
  if key_bytes.len() != 32 {
    return Err(EncryptError::InvalidChaChaKeyLength);
  }
  if nonce.len() != 12 {
    return Err(EncryptError::InvalidChaChaNonceLength);
  }
  // RFC 8439 caps plaintext length per nonce at 2^32 * 64 - 64 bytes.
  if data.len() as u64 > ((1u64 << 32) - 1) * 64 {
    return Err(EncryptError::TooMuchData);
  }

  let unbound_key = UnboundKey::new(&CHACHA20_POLY1305, key_bytes)
    .map_err(|_| EncryptError::Failed)?;
  let sealing_key = LessSafeKey::new(unbound_key);
  let aws_nonce = AwsNonce::try_assume_unique_for_key(nonce)
    .map_err(|_| EncryptError::Failed)?;
  let aad = additional_data.unwrap_or_default();

  let mut in_out = data.to_vec();
  sealing_key
    .seal_in_place_append_tag(aws_nonce, Aad::from(&aad), &mut in_out)
    .map_err(|_| EncryptError::Failed)?;

  Ok(in_out)
}

fn encrypt_aes_ctr_gen<B>(
  key: &[u8],
  counter: &[u8],
  data: &[u8],
) -> Result<Vec<u8>, EncryptError>
where
  B: KeyIvInit + StreamCipher,
{
  let mut cipher = B::new_from_slices(key, counter)
    .map_err(|_| EncryptError::InvalidKeyOrIv)?;

  let mut ciphertext = data.to_vec();
  cipher
    .try_apply_keystream(&mut ciphertext)
    .map_err(|_| EncryptError::TooMuchData)?;

  Ok(ciphertext)
}

pub(crate) fn encrypt_aes_ctr(
  key: &RawKeyData,
  key_length: usize,
  counter: &[u8],
  ctr_length: usize,
  data: &[u8],
) -> Result<Vec<u8>, EncryptError> {
  let key = key.as_secret_key()?;

  match ctr_length {
    32 => match key_length {
      128 => encrypt_aes_ctr_gen::<Ctr32BE<aes::Aes128>>(key, counter, data),
      192 => encrypt_aes_ctr_gen::<Ctr32BE<aes::Aes192>>(key, counter, data),
      256 => encrypt_aes_ctr_gen::<Ctr32BE<aes::Aes256>>(key, counter, data),
      _ => Err(EncryptError::InvalidLength),
    },
    64 => match key_length {
      128 => encrypt_aes_ctr_gen::<Ctr64BE<aes::Aes128>>(key, counter, data),
      192 => encrypt_aes_ctr_gen::<Ctr64BE<aes::Aes192>>(key, counter, data),
      256 => encrypt_aes_ctr_gen::<Ctr64BE<aes::Aes256>>(key, counter, data),
      _ => Err(EncryptError::InvalidLength),
    },
    128 => match key_length {
      128 => encrypt_aes_ctr_gen::<Ctr128BE<aes::Aes128>>(key, counter, data),
      192 => encrypt_aes_ctr_gen::<Ctr128BE<aes::Aes192>>(key, counter, data),
      256 => encrypt_aes_ctr_gen::<Ctr128BE<aes::Aes256>>(key, counter, data),
      _ => Err(EncryptError::InvalidLength),
    },
    _ => Err(EncryptError::InvalidCounterLength),
  }
}
