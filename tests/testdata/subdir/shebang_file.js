#!/usr/bin/env -S dodo run --allow-read

for (const item of Deno.readDirSync(".")) {
  console.log(item.name);
}
