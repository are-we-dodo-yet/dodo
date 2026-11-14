#!/usr/bin/env -S dodo run -A
// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

import { Repos } from "./repos.ts";
import { $ } from "@deno/rust-automation";

const repos = await Repos.load();

for (const crate of repos.getCrates()) {
  $.logStep(`Building ${crate.name}...`);
  await crate.build({ allFeatures: true });
}
