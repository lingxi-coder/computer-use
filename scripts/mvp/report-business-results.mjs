#!/usr/bin/env node
import { readFileSync } from 'node:fs';
import { summarizeBusinessResults } from './business-results.mjs';

if (process.argv.length !== 3) {
  process.stderr.write('Usage: node scripts/mvp/report-business-results.mjs <report.json>\n');
  process.exitCode = 2;
} else {
  try {
    const report = JSON.parse(readFileSync(process.argv[2], 'utf8'));
    process.stdout.write(`${JSON.stringify(summarizeBusinessResults(report), null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
