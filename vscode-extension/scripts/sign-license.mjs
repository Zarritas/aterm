#!/usr/bin/env node
// Licencias Pro de Agent Sessions — generación de claves y emisión/validación
// de licencias, offline (Ed25519). No requiere servidor.
//
//   node scripts/sign-license.mjs keygen
//       Genera un par Ed25519. Guarda la privada en license-private.pem
//       (gitignored) y escribe la pública en src/license.ts (LICENSE_PUBLIC_KEY).
//
//   node scripts/sign-license.mjs sign <email> [días]
//       Emite una licencia firmada. Sin «días» = perpetua; con «días» caduca.
//       Imprime la clave  ATERM-PRO.<payload>.<firma>  para dársela al cliente.
//
//   node scripts/sign-license.mjs verify <clave>
//       Valida una clave contra la pública embebida en src/license.ts.
//
// IMPORTANTE: license-private.pem es tu secreto de firma. Haz copia de
// seguridad y NO lo subas al repo (está en .gitignore).

import {
  generateKeyPairSync,
  createPrivateKey,
  createPublicKey,
  sign as edSign,
  verify as edVerify,
} from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(here, "..");
const PRIV_PATH = path.join(ROOT, "license-private.pem");
const LICENSE_TS = path.join(ROOT, "src", "license.ts");
const PREFIX = "ATERM-PRO.";

const b64url = (buf) =>
  Buffer.from(buf).toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const b64urlToBuf = (s) =>
  Buffer.from(s.replace(/-/g, "+").replace(/_/g, "/"), "base64");

function readPublicKeyB64() {
  const src = fs.readFileSync(LICENSE_TS, "utf8");
  const m = src.match(/const LICENSE_PUBLIC_KEY = "([^"]*)";/);
  if (!m) throw new Error("No encuentro LICENSE_PUBLIC_KEY en src/license.ts");
  return m[1];
}

function keygen() {
  if (fs.existsSync(PRIV_PATH)) {
    console.error(
      `Ya existe ${PRIV_PATH}. Bórralo a mano si de verdad quieres rotar la clave\n` +
        "(invalidará todas las licencias ya emitidas)."
    );
    process.exit(1);
  }
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const privPem = privateKey.export({ type: "pkcs8", format: "pem" });
  const pubB64 = publicKey.export({ type: "spki", format: "der" }).toString("base64");
  fs.writeFileSync(PRIV_PATH, privPem, { mode: 0o600 });

  const src = fs.readFileSync(LICENSE_TS, "utf8");
  const next = src.replace(
    /const LICENSE_PUBLIC_KEY = "[^"]*";/,
    `const LICENSE_PUBLIC_KEY = "${pubB64}";`
  );
  if (next === src) throw new Error("No pude escribir la pública en src/license.ts");
  fs.writeFileSync(LICENSE_TS, next);

  console.log("✓ Par Ed25519 generado.");
  console.log(`  privada → ${PRIV_PATH}  (gitignored, ¡haz backup!)`);
  console.log("  pública → escrita en src/license.ts (LICENSE_PUBLIC_KEY)");
  console.log("\nRecompila la extensión para que la validación entre en vigor.");
}

function sign(email, days) {
  if (!email) {
    console.error("Uso: sign <email> [días]");
    process.exit(1);
  }
  if (!fs.existsSync(PRIV_PATH)) {
    console.error(`Falta ${PRIV_PATH}. Ejecuta primero: node scripts/sign-license.mjs keygen`);
    process.exit(1);
  }
  const priv = createPrivateKey(fs.readFileSync(PRIV_PATH));
  const payload = { email };
  if (days) payload.exp = Math.floor(Date.now() / 1000) + Number(days) * 86400;
  const payloadB64 = b64url(JSON.stringify(payload));
  const sigB64 = b64url(edSign(null, Buffer.from(payloadB64), priv));
  const key = `${PREFIX}${payloadB64}.${sigB64}`;
  console.log(key);
  console.error(
    `\n(emitida para ${email}${days ? `, caduca en ${days} días` : ", perpetua"})`
  );
}

function verify(key) {
  if (!key) {
    console.error("Uso: verify <clave>");
    process.exit(1);
  }
  const pubB64 = readPublicKeyB64();
  if (!pubB64) {
    console.error("✗ LICENSE_PUBLIC_KEY está vacío en src/license.ts (ejecuta keygen).");
    process.exit(1);
  }
  if (!key.startsWith(PREFIX)) return fail("prefijo incorrecto");
  const [payloadB64, sigB64] = key.slice(PREFIX.length).split(".");
  if (!payloadB64 || !sigB64) return fail("formato incorrecto");
  const pub = createPublicKey({
    key: Buffer.from(pubB64, "base64"),
    format: "der",
    type: "spki",
  });
  if (!edVerify(null, Buffer.from(payloadB64), pub, b64urlToBuf(sigB64)))
    return fail("firma inválida");
  const payload = JSON.parse(b64urlToBuf(payloadB64).toString("utf8"));
  if (typeof payload.exp === "number" && Date.now() / 1000 > payload.exp)
    return fail("caducada");
  console.log("✓ válida", JSON.stringify(payload));
}
function fail(why) {
  console.error("✗ inválida:", why);
  process.exit(1);
}

const [cmd, a, b] = process.argv.slice(2);
if (cmd === "keygen") keygen();
else if (cmd === "sign") sign(a, b);
else if (cmd === "verify") verify(a);
else {
  console.error("Comandos: keygen | sign <email> [días] | verify <clave>");
  process.exit(1);
}
