const { stdout } = await new Deno.Command("deno", {
  args: ["eval", "console.log(Deno.env.get('LD_DENO_TEST'))"],
}).output();
console.log(new TextDecoder().decode(stdout).trim());
