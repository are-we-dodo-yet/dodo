// Copyright 2018-2026 the Deno authors. MIT license.

// Copied from https://github.com/littledivy/aead-gcm-stream (MIT) because
// RustCrypto has no incremental AEAD API yet:
// https://github.com/RustCrypto/traits/issues/1364

use aes::cipher::Array;
use aes::cipher::BlockCipherEncrypt;
use aes::cipher::BlockSizeUser;
use aes::cipher::InnerIvInit;
use aes::cipher::Key;
use aes::cipher::KeyInit;
use aes::cipher::KeySizeUser;
use aes::cipher::StreamCipher;
use aes::cipher::StreamCipherSeek;
use aes::cipher::consts::U16;
use ctr::Ctr32BE;
use ctr::CtrCore;
use ghash::GHash;
use ghash::universal_hash::UniversalHash;

const BLOCK_SIZE: usize = 16;

#[derive(Clone)]
struct GcmGhash {
  ghash: GHash,
  ghash_pad: [u8; BLOCK_SIZE],
  msg_buf: [u8; BLOCK_SIZE],
  msg_buf_offset: usize,
  ad_len: usize,
  msg_len: usize,
}

impl GcmGhash {
  fn new(h: &ghash::Key, ghash_pad: [u8; BLOCK_SIZE]) -> Self {
    Self {
      ghash: GHash::new(h),
      ghash_pad,
      msg_buf: [0u8; BLOCK_SIZE],
      msg_buf_offset: 0,
      ad_len: 0,
      msg_len: 0,
    }
  }

  fn set_aad(&mut self, aad: &[u8]) {
    self.ad_len = aad.len();
    self.ghash.update_padded(aad);
  }

  fn update(&mut self, mut msg: &[u8]) {
    if self.msg_buf_offset > 0 {
      let taking = msg.len().min(BLOCK_SIZE - self.msg_buf_offset);
      self.msg_buf[self.msg_buf_offset..self.msg_buf_offset + taking]
        .copy_from_slice(&msg[..taking]);
      self.msg_buf_offset += taking;
      self.msg_len += taking;

      if self.msg_buf_offset < BLOCK_SIZE {
        return;
      }

      self.ghash.update(&[ghash::Block::from(self.msg_buf)]);
      self.msg_buf_offset = 0;
      msg = &msg[taking..];
    }

    self.msg_len += msg.len();

    let (blocks, leftover) = ghash::Block::slice_as_chunks(msg);
    if !blocks.is_empty() {
      self.ghash.update(blocks);
    }

    self.msg_buf[..leftover.len()].copy_from_slice(leftover);
    self.msg_buf_offset = leftover.len();
  }

  fn finalize(mut self) -> ghash::Tag {
    if self.msg_buf_offset > 0 {
      self
        .ghash
        .update_padded(&self.msg_buf[..self.msg_buf_offset]);
    }

    let mut final_block = ghash::Block::default();
    final_block[..8].copy_from_slice(&(8 * self.ad_len as u64).to_be_bytes());
    final_block[8..].copy_from_slice(&(8 * self.msg_len as u64).to_be_bytes());

    self.ghash.update(&[final_block]);
    let mut hash = self.ghash.finalize();

    for (b, pad) in hash.iter_mut().zip(self.ghash_pad) {
      *b ^= pad;
    }

    hash
  }
}

pub struct AesGcm<Aes>
where
  Aes: BlockCipherEncrypt + BlockSizeUser<BlockSize = U16>,
{
  ctr: Ctr32BE<Aes>,
  ghash: GcmGhash,
}

impl<Aes> KeySizeUser for AesGcm<Aes>
where
  Aes: KeySizeUser + BlockCipherEncrypt + BlockSizeUser<BlockSize = U16>,
{
  type KeySize = Aes::KeySize;
}

impl<Aes> AesGcm<Aes>
where
  Aes: BlockCipherEncrypt + BlockSizeUser<BlockSize = U16> + KeyInit,
{
  pub fn new(key: &Key<Self>, nonce: &[u8]) -> Self {
    let cipher = Aes::new(key);
    let mut ghash_key = ghash::Key::default();
    cipher.encrypt_block(&mut ghash_key);

    let mut nonce_block = Array::<u8, U16>::default();
    if nonce.len() == 12 {
      nonce_block[..nonce.len()].copy_from_slice(nonce);
    } else {
      let mut ghash = GHash::new(&ghash_key);
      ghash.update_padded(nonce);
      ghash.update_padded(&(8 * nonce.len() as u128).to_be_bytes());
      nonce_block.copy_from_slice(&ghash.finalize());
      for i in nonce_block.iter_mut().rev() {
        *i = i.wrapping_sub(1);
        if *i != 0xff {
          break;
        }
      }
    }
    let mut ctr =
      Ctr32BE::from_core(CtrCore::inner_iv_init(cipher, &nonce_block));
    ctr.seek(Aes::block_size());

    let mut pad = [0u8; BLOCK_SIZE];
    ctr.apply_keystream(&mut pad);

    let ghash = GcmGhash::new(&ghash_key, pad);
    Self { ctr, ghash }
  }

  pub fn set_aad(&mut self, aad: &[u8]) {
    self.ghash.set_aad(aad);
  }

  pub fn encrypt(&mut self, block: &mut [u8]) {
    self.ctr.apply_keystream(block);
    self.ghash.update(block);
  }

  pub fn decrypt(&mut self, block: &mut [u8]) {
    self.ghash.update(block);
    self.ctr.apply_keystream(block);
  }

  pub fn finish(self) -> ghash::Tag {
    self.ghash.finalize()
  }
}

#[cfg(test)]
mod tests {
  use aes::Aes128;
  use aes::Aes256;

  use super::AesGcm;

  fn hex(s: &str) -> Vec<u8> {
    data_encoding::HEXLOWER.decode(s.as_bytes()).unwrap()
  }

  struct Vector {
    key: &'static str,
    nonce: &'static str,
    plaintext: &'static str,
    aad: &'static str,
    ciphertext: &'static str,
    tag: &'static str,
  }

  const AES128_VECTORS: &[Vector] = &[
    Vector {
      key: "11754cd72aec309bf52f7687212e8957",
      nonce: "3c819d9a9bed087615030b65",
      plaintext: "",
      aad: "",
      ciphertext: "",
      tag: "250327c674aaf477aef2675748cf6971",
    },
    Vector {
      key: "55c8bcb0021090e4b2c785c79cb966b8",
      nonce: "5e9f1313282f73d7ffb92837",
      plaintext: "2d7c1b689189bbfa2be26ad5c1f296dee4c0f61456ffc94cf8e70aad0f09d0608c4115aa6ed5eba93ed5820b3f3426bbf4d64a",
      aad: "f7e14a57e3bb6b99866b90573d7bc355baeb7ac347e43d0b65d97ecc2eb9c772401a8e3c7e9e2871c2b79579d44c139e62c33b42a9e0c87686960009d659d5e3874e168c334b6650c6d36168633757a7c20764232ce94a0de1a5",
      ciphertext: "ba59002df3394c5b80983519dc163eca5c44df80f8c4c4e15d3ff73f13c170c80a59d87a2165a7b450be01031a8e41c505c89f",
      tag: "28418c564731bddf3d504d8ed32e66ee",
    },
    Vector {
      key: "83f9d97d4ab759fddcc3ef54a0e2a8ec",
      nonce: "cf",
      plaintext: "77e6329cf9424f71c808df9170bfd298",
      aad: "6dd49eaeb4103dac8f97e3234946dd2d",
      ciphertext: "50de86a7a92a8a5ea33db5696b96cd77",
      tag: "aa181e84bc8b4bf5a68927c409d422cb",
    },
    Vector {
      key: "ca91e2414409a439b06573d772f90afb",
      nonce: "177008f920a06169ccdf753a338553fefd46845869c9244da44997f83d4ce805a18707c84d114f9c68427b22841591e6caecf5c3e72a25167aa860c51bdc1aa56dcd69f29a2f35e70a322b9eba092a98d66a956b4d294383a0ebab26f7c4df1a5d4060dfc45a14155100ea7d9e32debb6537406b757291710505142e7659fc77",
      plaintext: "28003e30c4a4ca9e41aafefac1e1c3de",
      aad: "bfeb15fcf7b15f0e14c04439b67950bd",
      ciphertext: "00e472971f3a7770aa7158fd92f17bb7",
      tag: "16661b85eb51646c94cf2be4e42d7a8e",
    },
  ];

  const AES256_VECTORS: &[Vector] = &[
    Vector {
      key: "b52c505a37d78eda5dd34f20c22540ea1b58963cf8e5bf8ffa85f9f2492505b4",
      nonce: "516c33929df5a3284ff463d7",
      plaintext: "",
      aad: "",
      ciphertext: "",
      tag: "bdc1ac884d332457a1d2664f168c76f0",
    },
    Vector {
      key: "6be7f4d18ff0fbdd9b3b3cacaba4629a0c617387079add62f6ce1584b33faad1",
      nonce: "fda568c9cb13d9c176bcef03",
      plaintext: "4df668e99d5068604a48bcca5baa8245435928558a83d68d7b0b081861224e9bd39ea8f2d55a635949e66c6f6a7ff5cc34dd94",
      aad: "11ebeb97dd4a9925c1fbe2b9af77392058d2d971e42db15da39f090d7bc132573c34bf7d92a2d72dc66ee6840c3ff07985b8976ee8d8f36bf47ae330b899fdc60652dd5a23c45f3680f11951f019e0697c8acfcaa95f01b9c7dd",
      ciphertext: "488b40ad594e1845ccdd9e9467fc5e1afbbfde34e57d45bfcd30b61cc326d57fe8e3f31a39cdebf00f60bbd2c3cdf69f756eff",
      tag: "3bf3fbab9b48486fd08a5552604df639",
    },
    Vector {
      key: "bb4635d766dd0e4a7019d1724c736e1f2c016af9e29e7d3aa2c0de23e780af26",
      nonce: "ab",
      plaintext: "d05ce878d94662d1520b184b4bef3c45",
      aad: "0f85c7dbeb674b7a70c35125d3619350",
      ciphertext: "51baa26a6a719c1600645ff3bfdfa53b",
      tag: "6bd54e5184eb300934b392c32b7c1a6e",
    },
    Vector {
      key: "fcbc7eb62716dc7f792b6194d26d6d569eaee07a9d3c37ca42854090661e1845",
      nonce: "4c8c4624279b23b495c788844c76d225ebf23826599c3e1cf4db1da2d65a7f7544d8e86fcc33fb113d3174b8c7903122cb5967f6107382cc5ac6e7a0e4ca4f08de3e911d483e68253d3f886cfe349bf93299a28e665bc096a51ce84ce6940b34a037722483b96a7b25507f5a04643c6730faaab618e6231a727714d6f366fa9b",
      plaintext: "22144fc12f7bc5522b88b76c8ded1c76",
      aad: "3c182af19c46ff4acbdacecf70b42fb5",
      ciphertext: "c8d98107c0cb3c0fd2189ae97280d562",
      tag: "2906772330ecd9a3b8a82876a4ebdeea",
    },
  ];

  macro_rules! check_vectors {
    ($aes:ty, $vectors:expr) => {
      for v in $vectors {
        let key = hex(v.key);
        let key = key.as_slice().try_into().unwrap();
        let nonce = hex(v.nonce);
        let aad = hex(v.aad);

        let mut data = hex(v.plaintext);
        let mut cipher = AesGcm::<$aes>::new(key, &nonce);
        cipher.set_aad(&aad);
        cipher.encrypt(&mut data);
        assert_eq!(data, hex(v.ciphertext));
        assert_eq!(cipher.finish().as_slice(), hex(v.tag));

        let mut cipher = AesGcm::<$aes>::new(key, &nonce);
        cipher.set_aad(&aad);
        cipher.decrypt(&mut data);
        assert_eq!(data, hex(v.plaintext));
        assert_eq!(cipher.finish().as_slice(), hex(v.tag));

        let mut data = hex(v.plaintext);
        let mut cipher = AesGcm::<$aes>::new(key, &nonce);
        cipher.set_aad(&aad);
        for chunk in data.chunks_mut(7) {
          cipher.encrypt(chunk);
        }
        assert_eq!(data, hex(v.ciphertext));
        assert_eq!(cipher.finish().as_slice(), hex(v.tag));
      }
    };
  }

  #[test]
  fn aes128_vectors() {
    check_vectors!(Aes128, AES128_VECTORS);
  }

  #[test]
  fn aes256_vectors() {
    check_vectors!(Aes256, AES256_VECTORS);
  }

  #[test]
  fn encrypt_no_data() {
    let mut cipher = AesGcm::<Aes128>::new(&[0; 16].into(), &[0; 12]);
    cipher.set_aad(&[0; 5]);
    assert_eq!(
      cipher.finish().as_slice(),
      &[
        160, 56, 248, 75, 148, 220, 251, 4, 34, 185, 85, 34, 190, 206, 136, 0
      ]
    );
  }

  #[test]
  fn encrypt_decrypt_1_data_block() {
    const CT: [u8; 16] = [
      3, 136, 218, 206, 96, 182, 163, 146, 243, 40, 194, 185, 113, 178, 254,
      120,
    ];
    const TAG: [u8; 16] = [
      83, 180, 67, 81, 66, 78, 216, 216, 225, 252, 47, 199, 8, 126, 112, 133,
    ];

    let mut data = [0u8; 16];
    let mut cipher = AesGcm::<Aes128>::new(&[0; 16].into(), &[0; 12]);
    cipher.set_aad(&[0; 5]);
    cipher.encrypt(&mut data);
    assert_eq!(cipher.finish().as_slice(), &TAG);
    assert_eq!(data, CT);

    let mut cipher = AesGcm::<Aes128>::new(&[0; 16].into(), &[0; 12]);
    cipher.set_aad(&[0; 5]);
    cipher.decrypt(&mut data);
    assert_eq!(cipher.finish().as_slice(), &TAG);
    assert_eq!(data, [0u8; 16]);
  }

  #[test]
  fn decrypt_padded_data() {
    let mut data = [0x03, 0x88, 0xDA, 0xCE, 0x60, 0xB6, 0xA3];
    let mut cipher = AesGcm::<Aes128>::new(&[0; 16].into(), &[0; 12]);
    cipher.set_aad(&[0; 5]);
    cipher.decrypt(&mut data);
    assert_eq!(
      cipher.finish().as_slice(),
      &[
        9, 75, 180, 34, 99, 68, 253, 148, 92, 120, 157, 203, 191, 156, 155, 79
      ]
    );
    assert_eq!(data, [0u8; 7]);
  }

  #[test]
  fn encrypt_stream() {
    let mut cipher = AesGcm::<Aes128>::new(&[0; 16].into(), &[0; 12]);
    cipher.set_aad(&[]);
    let mut p1 = b"hello".to_vec();
    cipher.encrypt(&mut p1);
    let mut p2 = b"world".to_vec();
    cipher.encrypt(&mut p2);

    assert_eq!(p1, hex("6bedb6a20f"));
    assert_eq!(p2, hex("c1cce09f4c"));
    assert_eq!(
      cipher.finish().as_slice(),
      hex("bf6d20a38e0c828bea3de63b7ff1dfbd")
    );
  }
}
