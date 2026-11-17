Deno.env.set("LD_DENO_TEST", "1");

try {
  await new Deno.Command("dodo", { args: ["eval", ""] }).output();
} catch (err) {
  console.log((err as Error).message);
}
