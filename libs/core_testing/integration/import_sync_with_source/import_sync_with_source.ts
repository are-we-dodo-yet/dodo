// Copyright 2018-2026 the Deno authors. MIT license.

const { op_import_sync_with_source, op_path_to_url } = Deno.core.ops;

const resolve = (p: string) => op_path_to_url(p);

console.log(
  op_import_sync_with_source(
    resolve("./integration/import_sync_with_source/main.js"),
    'export { value } from "./dep.js";\nexport const hooked = true;\n',
  ),
);
