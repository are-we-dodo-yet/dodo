// Copyright 2018-2026 the Deno authors. MIT license.
use core::fmt;

use digest::HashMarker;
use digest::Output;
use digest::block_api::AlgorithmName;
use digest::block_api::Block;
use digest::block_api::BlockSizeUser;
use digest::block_api::Buffer;
use digest::block_api::BufferKindUser;
use digest::block_api::Eager;
use digest::block_api::FixedOutputCore;
use digest::block_api::OutputSizeUser;
use digest::block_api::Reset;
use digest::block_api::UpdateCore;
use md5::block_api::Md5Core;
use sha1::block_api::Sha1Core;

digest::buffer_fixed!(
  pub struct Md5Sha1(Md5Sha1Core);
  impl: BaseFixedTraits AlgorithmName Default Clone HashMarker Reset FixedOutputReset;
);

pub struct Md5Sha1Core {
  md5: Md5Core,
  sha1: Sha1Core,
}

impl HashMarker for Md5Sha1Core {}

impl BlockSizeUser for Md5Sha1Core {
  type BlockSize = digest::consts::U64;
}

impl BufferKindUser for Md5Sha1Core {
  type BufferKind = Eager;
}

impl OutputSizeUser for Md5Sha1Core {
  type OutputSize = digest::consts::U36;
}

impl UpdateCore for Md5Sha1Core {
  #[inline]
  fn update_blocks(&mut self, blocks: &[Block<Self>]) {
    self.md5.update_blocks(blocks);
    self.sha1.update_blocks(blocks);
  }
}

impl FixedOutputCore for Md5Sha1Core {
  #[inline]
  fn finalize_fixed_core(
    &mut self,
    buffer: &mut Buffer<Self>,
    out: &mut Output<Self>,
  ) {
    let mut md5_output = Output::<Md5Core>::default();
    self
      .md5
      .finalize_fixed_core(&mut buffer.clone(), &mut md5_output);
    let mut sha1_output = Output::<Sha1Core>::default();
    self.sha1.finalize_fixed_core(buffer, &mut sha1_output);
    out[..16].copy_from_slice(&md5_output);
    out[16..].copy_from_slice(&sha1_output);
  }
}

impl Default for Md5Sha1Core {
  #[inline]
  fn default() -> Self {
    Self {
      md5: Default::default(),
      sha1: Default::default(),
    }
  }
}

impl Clone for Md5Sha1Core {
  #[inline]
  fn clone(&self) -> Self {
    Self {
      md5: self.md5.clone(),
      sha1: self.sha1.clone(),
    }
  }
}

impl Reset for Md5Sha1Core {
  #[inline]
  fn reset(&mut self) {
    *self = Default::default();
  }
}

impl AlgorithmName for Md5Sha1Core {
  fn write_alg_name(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Md5Sha1")
  }
}

impl fmt::Debug for Md5Sha1Core {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Md5Sha1Core { ... }")
  }
}
