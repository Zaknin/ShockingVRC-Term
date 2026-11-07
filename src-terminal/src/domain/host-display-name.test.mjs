import test from "node:test";
import assert from "node:assert/strict";
import { displayHostName } from "./host-display-name.mjs";

test("authenticated Host name replaces only the generated local default alias", () => {
  assert.equal(displayHostName({ localAlias: "Host 12345678", hostDisplayName: "Nightmare's Coyote", serverId: "12345678-host" }), "Nightmare's Coyote");
  assert.equal(displayHostName({ localAlias: "Nightmare — Main", hostDisplayName: "Nightmare's Coyote", serverId: "12345678-host" }), "Nightmare — Main");
  assert.equal(displayHostName({ localAlias: "", hostDisplayName: "Nightmare's Coyote", serverId: "12345678-host" }), "Nightmare's Coyote");
  assert.equal(displayHostName({ localAlias: "Host 12345678", serverId: "12345678-host" }), "Host 12345678");
});

test("explicit local aliases take precedence even when they match the generated fallback", () => {
  assert.equal(displayHostName({ localAlias: "Host 12345678", hasLocalAlias: true, hostDisplayName: "My Coyote", serverId: "12345678-host" }), "Host 12345678");
  assert.equal(displayHostName({ localAlias: "Nightmare — Main", hasLocalAlias: true, hostDisplayName: "My Coyote", serverId: "12345678-host" }), "Nightmare — Main");
});
