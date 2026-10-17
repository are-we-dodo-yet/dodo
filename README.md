# Dodo

[![Discord badge][]][Discord link]

A community fork of [Deno](https://github.com/denoland/deno):

> [Deno](https://deno.com)
> ([/ˈdiːnoʊ/](https://ipa-reader.com/?text=%CB%88di%CB%90no%CA%8A), pronounced
> `dee-no`) is a JavaScript, TypeScript, and WebAssembly runtime with secure
> defaults and a great developer experience. It's built on
> [V8](https://v8.dev/), [Rust](https://www.rust-lang.org/), and
> [Tokio](https://tokio.rs/).

<img align="right" src="https://github.com/are-we-dodo-yet/branding/blob/main/Mark%20only/Dark%20(outlined)/Transparent/mark_dark_transparent.png?raw=true" height="150px" alt="the dodo mascot">

> [!IMPORTANT]
> On October 9, 2026, Deno [announced](https://deno.com/blog/cloudflare) that
> its team is
> [joining Cloudflare](https://blog.cloudflare.com/deno-joins-cloudflare/). The
> Deno team will keep shipping monthly bug fixes and security releases of the
> runtime for one more year, and then stop developing it. Deno stays open
> source, and the announcement invites others to continue it. Dodo is one of
> these efforts.

## Status

**Very Early.** There are no Dodo releases yet. Complete instructions for
building Dodo from source can be found
[here](.github/CONTRIBUTING.md#building-from-source).

## Why fork?

Deno's core lives across multiple `denoland` repos (`deno_graph`, `deno_ast`,
`deno_doc`, `deno_lint`, `deno_media_type`, and more), which reach _this_ repo
via pinned crates.io releases. This means that each change has to be merged and
released to multiple packages, which makes outside contributions very difficult.

We also plan to compartmentalize many of Deno's auxiliary features _(a.k.a.
anything not critical to Deno's internal libraries)_ as **dynamically-linked
extensions**. The hope is that this will allow the community to decentralize the
maintenance burden and enable developers to keep their runtimes lean.

### Current Roadmap

#### Now

- **[IN PROGRESS]** Get the Monorepo stable, with passing CI.
- **[IN PROGRESS]** Triage top Deno issues in
  [our own project board](https://github.com/orgs/are-we-dodo-yet/projects/2).
- Release our first build!
- Setup a basic fundraising mechanism.

#### Near

- **[IN PROGRESS]** A proposal for Dodo Extensions emerges.
- The Dodo Volunteers are equipped to take in security patches as Deno releases
  them.
- Dodo becomes the most-starred Deno fork 🤞

#### Far

- Dodo Volunteers are equipped to author their _own_ security patches and
  dependency updates (like support for new releases of v8 and TypeScript)
- Deno Desktop adds support for Mobile.
- Dodo pursues official buy-in from organizations previously dependent on Deno.

## Your first Dodo program

Dodo can be used for many different applications, but is most commonly used to
build web servers. Create a file called `server.ts` and include the following
TypeScript code:

```ts
const Dodo = Deno;

Dodo.serve((_req: Request) => {
  return new Response("Hello, world!");
});
```

Run your server with the following command:

```sh
dodo run --allow-net server.ts
```

This should start a local web server on
[http://localhost:8000](http://localhost:8000).

Learn more about writing and running Dodo programs
[in the docs](https://docs.deno.com/runtime/manual).

## Additional resources

- **[Deno Docs](https://docs.deno.com)**: official guides and reference docs for
  the Deno runtime, [Deno Deploy](https://deno.com/deploy), and beyond.
- **[Deno Standard Library](https://jsr.io/@std)**: officially supported common
  utilities for Deno programs.
- **[JSR](https://jsr.io/)**: The open-source package registry for modern
  JavaScript and TypeScript
- **[Developer Blog](https://deno.com/blog)**: Product updates, tutorials, and
  more from the Deno team.

## Contributing

We appreciate your help! Open pull requests against the `trunk` branch of
[are-we-dodo-yet/dodo](https://github.com/are-we-dodo-yet/dodo), and please read
the [contributing instructions](.github/CONTRIBUTING.md).

[Discord badge]: https://img.shields.io/discord/1558888938251223050?logo=discord&style=social
[Discord link]: https://discord.gg/sX7R8Pqnp
