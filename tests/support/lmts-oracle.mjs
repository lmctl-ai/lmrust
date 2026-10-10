#!/usr/bin/env node
/**
 * @fileoverview Node bridge script for lmrust differential tests.
 * Loads `@lmctl-ai/lmts` dist/ and answers requests from stdin as JSON lines.
 */

import readline from 'node:readline';
import path from 'node:path';
import fs from 'node:fs';
import { pathToFileURL } from 'node:url';

const lmtsDir = process.env.LMTS_DIR || process.argv[2] || path.resolve(process.cwd(), '../lmts');
const distDir = path.resolve(lmtsDir, 'dist');

if (!fs.existsSync(distDir)) {
  process.stderr.write(`Error: lmts dist directory not found at ${distDir}\n`);
  process.exit(1);
}

const { validate } = await import(pathToFileURL(path.join(distDir, 'validate.js')).href);
const { parseArgvForm } = await import(pathToFileURL(path.join(distDir, 'argv-form.js')).href);
const { parseJsonDocument } = await import(pathToFileURL(path.join(distDir, 'json-form.js')).href);
const { parseGraphqlDocument } = await import(pathToFileURL(path.join(distDir, 'graphql-form.js')).href);
const { encodeArgv, encodeJson, encodeGraphql } = await import(pathToFileURL(path.join(distDir, 'encode.js')).href);
const { interpretCommand } = await import(pathToFileURL(path.join(distDir, 'interpret.js')).href);

const rl = readline.createInterface({
  input: process.stdin,
  output: process.stdout,
  terminal: false,
});

rl.on('line', (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let req;
  try {
    req = JSON.parse(trimmed);
  } catch (err) {
    process.stdout.write(JSON.stringify({ id: -1, panic: `Invalid JSON line: ${err.message}` }) + '\n');
    return;
  }

  const { id, op, payload } = req;
  try {
    let resp;
    switch (op) {
      case 'ping': {
        resp = { id, ok: true, pong: true };
        break;
      }
      case 'validate': {
        const res = validate(payload.schema, payload.raw, payload.opts);
        resp = {
          id,
          ok: res.ok,
          value: res.ok ? res.value : undefined,
          issues: !res.ok ? res.issues : undefined,
        };
        break;
      }
      case 'parse_argv_form': {
        const res = parseArgvForm(payload.argv, payload.schema, payload.globals);
        resp = {
          id,
          ok: res.ok,
          value: res.ok ? res.value : undefined,
          issues: !res.ok ? res.issues : undefined,
        };
        break;
      }
      case 'parse_json_document': {
        const res = parseJsonDocument(payload.text);
        resp = {
          id,
          ok: res.ok,
          value: res.ok ? res.value : undefined,
          issues: !res.ok ? res.issues : undefined,
        };
        break;
      }
      case 'parse_graphql_document': {
        const res = parseGraphqlDocument(payload.text);
        resp = {
          id,
          ok: res.ok,
          value: res.ok ? res.value : undefined,
          issues: !res.ok ? res.issues : undefined,
        };
        break;
      }
      case 'encode_argv': {
        const res = encodeArgv(payload.schema, payload.params, payload.opts);
        resp = { id, ok: true, value: res };
        break;
      }
      case 'encode_json': {
        const res = encodeJson(payload.schema, payload.params);
        resp = { id, ok: true, value: res };
        break;
      }
      case 'encode_graphql': {
        try {
          const res = encodeGraphql(payload.schema, payload.params);
          resp = { id, ok: true, value: res };
        } catch (err) {
          resp = { id, ok: false, error: err.message };
        }
        break;
      }
      case 'interpret_command': {
        const schemas = Array.isArray(payload.schemas) ? payload.schemas : [payload.schemas];
        const res = interpretCommand(schemas, payload.line, { globals: payload.globals });
        resp = {
          id,
          ok: res.ok,
          value: res.ok ? res.value : undefined,
          issues: !res.ok ? res.issues : undefined,
        };
        break;
      }
      default: {
        resp = { id, panic: `Unknown op: ${op}` };
        break;
      }
    }
    process.stdout.write(JSON.stringify(resp) + '\n');
  } catch (err) {
    process.stdout.write(JSON.stringify({ id, panic: err.message, stack: err.stack }) + '\n');
  }
});
