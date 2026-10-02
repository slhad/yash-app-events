const assert = require('node:assert/strict');
const { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, readdirSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { resolve, join } = require('node:path');
const { spawnSync } = require('node:child_process');
const { test } = require('node:test');

test('optional XDG installation preserves shell settings and backs up updates', () => {
  const directory = mkdtempSync(join(tmpdir(), 'yash-plugin-install-'));
  try {
    const config = join(directory, 'config');
    const bin = join(directory, 'bin');
    mkdirSync(join(config, 'omarchy'), { recursive: true });
    mkdirSync(bin);
    const shell = '{"bar":{"layout":{"right":[{"id":"existing"}]}}}\n';
    writeFileSync(join(config, 'omarchy/shell.json'), shell);
    const log = join(directory, 'calls');
    writeFileSync(join(bin, 'omarchy'), '#!/bin/bash\nprintf "%s\\n" "$*" >> "$YASH_TEST_CALLS"\n', { mode: 0o755 });
    writeFileSync(join(bin, 'omarchy-shell'), '#!/bin/bash\nprintf "%s\\n" "$*" >> "$YASH_TEST_CALLS"\n', { mode: 0o755 });
    const env = { ...process.env, XDG_CONFIG_HOME: config, PATH: bin + ':' + process.env.PATH, YASH_TEST_CALLS: log };
    const script = resolve(__dirname, '../../../scripts/install-quickshell.sh');
    const run = args => spawnSync('bash', [script, ...args], { env, encoding: 'utf8' });
    assert.equal(run([]).status, 0);
    assert.equal(readFileSync(join(config, 'omarchy/shell.json'), 'utf8'), shell);
    const plugin = join(config, 'omarchy/plugins/io.github.yash-app-events.status');
    assert.equal(JSON.parse(readFileSync(join(plugin, 'manifest.json'), 'utf8')).schemaVersion, 1);
    assert.equal(readFileSync(join(plugin, 'yash-events.svg'), 'utf8'),
      readFileSync(resolve(__dirname, '../yash-events.svg'), 'utf8'));
    assert.doesNotMatch(readFileSync(log, 'utf8'), /bar put/);
    assert.equal(run(['--enable']).status, 0);
    assert.match(readFileSync(log, 'utf8'), /bar put io.github.yash-app-events.status --section right/);
    assert.match(readFileSync(log, 'utf8'), /shell rescanPlugins/);
    assert(readdirSync(join(config, 'omarchy')).some(name => name.startsWith('shell.json.yash-backup.')));
    assert(readdirSync(join(config, 'omarchy')).some(name => name.startsWith('yash-status-backup.')));
    assert.equal(run(['--bad']).status, 2);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
