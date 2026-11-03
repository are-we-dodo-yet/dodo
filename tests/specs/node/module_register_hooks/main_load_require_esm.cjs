const assert = require("assert");
const { registerHooks } = require("module");

const hook = registerHooks({
  load(url, context, nextLoad) {
    const result = nextLoad(url, context);
    if (url.endsWith("/esm_with_import/main.js")) {
      return {
        source: `${result.source}\nexport const hooked = true;\n`,
        format: "module",
        shortCircuit: true,
      };
    }
    return result;
  },
});

const mod = require("./esm_with_import/main.js");
assert.strictEqual(mod.value, 42);
assert.strictEqual(mod.hooked, true);
console.log("load hook require esm with imports works");

hook.deregister();
