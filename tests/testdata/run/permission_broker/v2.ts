// Opens files through a v2 permission broker (see test_permission_broker_v2).
const dir = Deno.args[0];
function attempt(name: string, fn: () => string) {
  try {
    console.log(`${name}: ${fn()}`);
  } catch (e) {
    console.log(`${name}: ${(e as Error).name}`);
  }
}
attempt("data.txt", () => Deno.readTextFileSync(`${dir}/data.txt`));
attempt("denied.txt", () => Deno.readTextFileSync(`${dir}/denied.txt`));
attempt("mismatch.txt", () => Deno.readTextFileSync(`${dir}/mismatch.txt`));
attempt("swapped.txt", () => Deno.readTextFileSync(`${dir}/swapped.txt`));
attempt("out.txt", () => {
  const file = Deno.openSync(`${dir}/out.txt`, { write: true, create: true });
  file.writeSync(new TextEncoder().encode("written"));
  file.close();
  return Deno.readTextFileSync(`${dir}/out.txt`);
});
console.log(`async data.txt: ${await Deno.readTextFile(`${dir}/data.txt`)}`);
