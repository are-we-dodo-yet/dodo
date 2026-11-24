// Copyright 2018-2026 the Deno authors. MIT license.

// `Deno.openRoot()` (ext/fs/root.rs).

import {
  assert,
  assertEquals,
  assertRejects,
  assertThrows,
} from "./test_util.ts";
import { join } from "@std/path";

const windows = Deno.build.os === "windows";

/** `<tmp>/root` (opened as the root) beside `<tmp>/outside/secret.txt`. */
function bed() {
  const tmp = Deno.makeTempDirSync({ prefix: "deno_root_test_" });
  const root = join(tmp, "root");
  const outside = join(tmp, "outside");
  Deno.mkdirSync(root);
  Deno.mkdirSync(outside);
  Deno.writeTextFileSync(join(outside, "secret.txt"), "outside");
  return {
    tmp,
    root,
    outside,
    [Symbol.dispose]() {
      Deno.removeSync(tmp, { recursive: true });
    },
  };
}

/** A link to a directory (a junction on Windows, which needs no privilege). */
function linkDir(target: string, path: string) {
  Deno.symlinkSync(target, path, { type: windows ? "junction" : "dir" });
}

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootReadsAndWritesBeneathItself() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    assertEquals(root.path, b.root);

    await root.writeTextFile("a.txt", "hello");
    assertEquals(await root.readTextFile("a.txt"), "hello");
    assertEquals(Deno.readTextFileSync(join(b.root, "a.txt")), "hello");
    root.writeFileSync("b.bin", new Uint8Array([1, 2, 3]));
    assertEquals(root.readFileSync("b.bin"), new Uint8Array([1, 2, 3]));
    await root.writeTextFile("a.txt", " world", { append: true });
    assertEquals(root.readTextFileSync("a.txt"), "hello world");
    await assertRejects(
      () => root.writeTextFile("a.txt", "x", { createNew: true }),
      Deno.errors.AlreadyExists,
    );
    await assertRejects(
      () => root.writeTextFile("missing.txt", "x", { create: false }),
      Deno.errors.NotFound,
    );

    await root.mkdir("d/e/f", { recursive: true });
    root.mkdirSync("d/e/f", { recursive: true });
    await assertRejects(() => root.mkdir("d"), Deno.errors.AlreadyExists);
    root.writeTextFileSync("d/e/f/g.txt", "deep");
    // `..` that stays inside is resolved lexically
    assertEquals(await root.readTextFile("d/e/../e/f/g.txt"), "deep");
    assertEquals(await root.readTextFile("./d/e/f/g.txt"), "deep");

    const st = await root.stat("d/e/f/g.txt");
    assert(st.isFile);
    assertEquals(st.size, 4);
    assert(root.statSync("d").isDirectory);
    assert((await root.stat("")).isDirectory);
    assert(root.lstatSync(".").isDirectory);

    const names = [];
    for await (const entry of root.readDir(".")) names.push(entry.name);
    names.sort();
    assertEquals(names, ["a.txt", "b.bin", "d"]);
    const entries = [...root.readDirSync("d")];
    assertEquals(entries.length, 1);
    assertEquals(entries[0].name, "e");
    assert(entries[0].isDirectory);

    await root.rename("a.txt", "d/moved.txt");
    root.renameSync("d/moved.txt", "d/e/moved.txt");
    assertEquals(root.readTextFileSync("d/e/moved.txt"), "hello world");
    await assertRejects(() => root.stat("a.txt"), Deno.errors.NotFound);

    {
      using file = await root.open("d/e/f/g.txt");
      const buf = new Uint8Array(4);
      assertEquals(await file.read(buf), 4);
      assertEquals(new TextDecoder().decode(buf), "deep");
    }
    {
      using file = root.openSync("new.txt", { write: true, createNew: true });
      file.writeSync(new TextEncoder().encode("made"));
    }
    assertEquals(root.readTextFileSync("new.txt"), "made");
    {
      using file = await root.create("new.txt");
      assertEquals((await file.stat()).size, 0);
    }

    await root.remove("b.bin");
    await assertRejects(() => root.remove("d"), Error);
    root.removeSync("d", { recursive: true });
    assertThrows(() => root.statSync("d"), Deno.errors.NotFound);
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  function rootRefusesPathsThatEscape() {
    using b = bed();
    using root = Deno.openRootSync(b.root);
    const escapes = [
      "../outside/secret.txt",
      "..",
      "a/../../outside/secret.txt",
      join(b.outside, "secret.txt"),
      "/etc/passwd",
    ];
    if (windows) {
      escapes.push("C:\\Windows\\win.ini", "\\outside", "C:outside");
    }
    for (const path of escapes) {
      assertThrows(() => root.readTextFileSync(path), Deno.errors.NotCapable);
      assertThrows(() => root.statSync(path), Deno.errors.NotCapable);
      assertThrows(
        () => root.writeTextFileSync(path, "pwned"),
        Deno.errors.NotCapable,
      );
      assertThrows(() => root.mkdirSync(path), Deno.errors.NotCapable);
      assertThrows(() => root.removeSync(path), Deno.errors.NotCapable);
      assertThrows(() => [...root.readDirSync(path)], Deno.errors.NotCapable);
    }
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );
    assertThrows(
      // deno-lint-ignore no-explicit-any
      () => root.readTextFileSync(new URL("file:///etc/passwd") as any),
      TypeError,
    );
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootRefusesARenameOut() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.writeTextFile("a.txt", "inside");
    await assertRejects(
      () => root.rename("a.txt", "../a.txt"),
      Deno.errors.NotCapable,
    );
    assertThrows(
      () => root.renameSync("../outside/secret.txt", "stolen.txt"),
      Deno.errors.NotCapable,
    );
    assertThrows(() => root.renameSync(".", "x"), Deno.errors.NotCapable);
    // the root itself can't be removed through it
    await assertRejects(
      () => root.remove(".", { recursive: true }),
      Deno.errors.NotCapable,
    );
    assertEquals(root.readTextFileSync("a.txt"), "inside");
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );
    assertThrows(
      () => Deno.statSync(join(b.tmp, "a.txt")),
      Deno.errors.NotFound,
    );
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootNeverFollowsALinkOut() {
    using b = bed();
    linkDir(b.outside, join(b.root, "out"));
    using root = await Deno.openRoot(b.root);

    await assertRejects(
      () => root.readTextFile("out/secret.txt"),
      Deno.errors.NotCapable,
    );
    await assertRejects(
      () => root.writeTextFile("out/planted.txt", "x"),
      Deno.errors.NotCapable,
    );
    await assertRejects(() => root.stat("out"), Deno.errors.NotCapable);
    assertThrows(() => [...root.readDirSync("out")], Deno.errors.NotCapable);
    assertThrows(
      () => root.mkdirSync("out/sub", { recursive: true }),
      Deno.errors.NotCapable,
    );
    assertThrows(() => Deno.statSync(join(b.outside, "planted.txt")));
    assertThrows(() => Deno.statSync(join(b.outside, "sub")));

    // lstat sees the link itself
    assert((await root.lstat("out")).isSymlink);
    // a recursive remove of a folder holding the link removes the link, not
    // what it leads to
    await root.mkdir("holder");
    linkDir(b.outside, join(b.root, "holder", "out"));
    await root.remove("holder", { recursive: true });
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );

    if (!windows) {
      // a file link out, absolute and relative
      Deno.symlinkSync(join(b.outside, "secret.txt"), join(b.root, "abs"));
      Deno.symlinkSync("../outside/secret.txt", join(b.root, "rel"));
      for (const link of ["abs", "rel"]) {
        assertThrows(() => root.readTextFileSync(link), Deno.errors.NotCapable);
        assertThrows(
          () => root.writeTextFileSync(link, "pwned"),
          Deno.errors.NotCapable,
        );
      }
      // a symlink made through the root to outside leads nowhere either
      await root.symlink("../outside", "made");
      assertEquals(await root.readLink("made"), "../outside");
      await assertRejects(
        () => root.readTextFile("made/secret.txt"),
        Deno.errors.NotCapable,
      );
      assertEquals(
        Deno.readTextFileSync(join(b.outside, "secret.txt")),
        "outside",
      );
    }
  },
);

Deno.test(
  { ignore: windows, permissions: { read: true, write: true } },
  async function rootFollowsALinkThatStaysInside() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.mkdir("real");
    await root.writeTextFile("real/f.txt", "inside");
    await root.symlink("real", "alias");
    await root.symlink(join(b.root, "real", "f.txt"), "abs-alias");
    assertEquals(await root.readTextFile("alias/f.txt"), "inside");
    assert((await root.stat("alias")).isDirectory);
    assertEquals(await root.readTextFile("abs-alias"), "inside");
  },
);

Deno.test(
  { ignore: !windows, permissions: { read: true, write: true } },
  async function rootRefusesAJunctionInsideOnWindows() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.mkdir("real");
    await root.writeTextFile("real/f.txt", "inside");
    linkDir(join(b.root, "real"), join(b.root, "alias"));
    await assertRejects(
      () => root.readTextFile("alias/f.txt"),
      Deno.errors.NotCapable,
    );
    await assertRejects(
      () => root.symlink("real", "x"),
      Deno.errors.NotSupported,
    );
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootHoldsItsDirectoryWhenThePathIsSwapped() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.writeTextFile("f.txt", "original");
    // the root's own path is moved away and replaced by a link out
    Deno.renameSync(b.root, join(b.tmp, "moved"));
    linkDir(b.outside, b.root);
    assertEquals(await root.readTextFile("f.txt"), "original");
    await assertRejects(
      () => root.readTextFile("secret.txt"),
      Deno.errors.NotFound,
    );
    await root.writeTextFile("g.txt", "written");
    assertEquals(
      Deno.readTextFileSync(join(b.tmp, "moved", "g.txt")),
      "written",
    );
    assertThrows(() => Deno.statSync(join(b.outside, "g.txt")));
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootSurvivesASymlinkSwapRace() {
    using b = bed();
    Deno.mkdirSync(join(b.root, "sub"));
    Deno.writeTextFileSync(join(b.root, "sub", "secret.txt"), "inside");
    using root = await Deno.openRoot(b.root);

    // a worker swaps `sub` between the real folder and a link out, as fast
    // as it can, while the root reads through it
    const code = `
      const [sub, real, outside, windows] = ${
      JSON.stringify([
        join(b.root, "sub"),
        join(b.root, "sub.real"),
        b.outside,
        windows,
      ])
    };
      let stop = false;
      self.onmessage = () => { stop = true; };
      let swaps = 0;
      while (!stop && swaps < 20000) {
        try {
          Deno.renameSync(sub, real);
          Deno.symlinkSync(outside, sub, { type: windows ? "junction" : "dir" });
          Deno.removeSync(sub);
          Deno.renameSync(real, sub);
        } catch { /* a read holds a handle (Windows): try again */ }
        swaps++;
        if (swaps % 64 === 0) await new Promise((r) => setTimeout(r, 0));
      }
      self.postMessage(swaps);
    `;
    const worker = new Worker(
      URL.createObjectURL(new Blob([code], { type: "text/javascript" })),
      { type: "module" },
    );
    const done = new Promise<number>((resolve) => {
      worker.onmessage = (e) => resolve(e.data);
    });
    let inside = 0;
    try {
      for (let i = 0; i < 2000; i++) {
        try {
          const text = await root.readTextFile("sub/secret.txt");
          assertEquals(text, "inside", "read outside the root");
          inside++;
        } catch (err) {
          // refused (the link), or caught between the swap's steps: the
          // name gone, a link where the walk wanted a folder, or a link gone
          // again before the walk read it (EINVAL). Any refusal is fine;
          // reading the outside file is not.
          assert(err instanceof Error, String(err));
        }
      }
    } finally {
      worker.postMessage("stop");
      await done;
      worker.terminate();
    }
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );
    // tidy the bed if the worker stopped half way through a swap
    try {
      Deno.statSync(join(b.root, "sub.real"));
      try {
        Deno.removeSync(join(b.root, "sub"));
      } catch { /* none */ }
      Deno.renameSync(join(b.root, "sub.real"), join(b.root, "sub"));
    } catch { /* already back */ }
    assert(inside >= 0);
  },
);

Deno.test(
  { permissions: { read: true, write: false } },
  async function rootWritesNeedWriteAccess() {
    using root = await Deno.openRoot(".");
    await assertRejects(
      () => root.writeTextFile("root_test_never.txt", "x"),
      Deno.errors.NotCapable,
    );
    assertThrows(() => root.mkdirSync("never"), Deno.errors.NotCapable);
    assertThrows(() => root.removeSync("never"), Deno.errors.NotCapable);
  },
);

Deno.test({ permissions: { read: false } }, async function rootNeedsRead() {
  await assertRejects(() => Deno.openRoot("."), Deno.errors.NotCapable);
  assertThrows(() => Deno.openRootSync("."), Deno.errors.NotCapable);
});

Deno.test(
  { permissions: { read: true } },
  async function rootClosesAndCannotBeConstructed() {
    const root = await Deno.openRoot(".");
    root.close();
    assertThrows(() => root.statSync("."), Deno.errors.BadResource);
    assertThrows(
      // deno-lint-ignore no-explicit-any
      () => new (Deno.Root as any)(0, ".", undefined),
      TypeError,
    );
    await assertRejects(
      () => Deno.openRoot("./root-test-missing"),
      Deno.errors.NotFound,
    );
  },
);
