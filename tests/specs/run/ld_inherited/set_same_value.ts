Deno.env.set("LD_DENO_TEST", "1");

try {
  await new Deno.Command("deno", { args: ["eval", ""] }).output();
} catch (err) {
  console.log((err as Error).message);
}
