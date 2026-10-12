try {
  await new Deno.Command("deno", {
    args: ["eval", ""],
    env: { "LD_DENO_TEST": "1" },
  }).output();
} catch (err) {
  console.log((err as Error).message);
}
