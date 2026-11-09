// Copyright 2018-2026 the Deno authors. MIT license.

use elliptic_curve::common::getrandom::SysRng;
use elliptic_curve::rand_core::UnwrapErr;

pub fn sys_rng() -> UnwrapErr<SysRng> {
  UnwrapErr(SysRng)
}
