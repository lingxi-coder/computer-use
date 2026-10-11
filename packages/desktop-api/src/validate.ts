import Ajv2020 from 'ajv/dist/2020.js';
import type { ValidateFunction } from 'ajv';

import type { Envelope, OperatorCommand } from './generated/envelope.js';
import { envelopeSchema } from './generated/schema.js';

/** Every `format` the schema uses. They are size hints from the Rust types, not string formats. */
function collectFormats(node: unknown, found = new Set<string>()): Set<string> {
  if (Array.isArray(node)) {
    for (const item of node) collectFormats(item, found);
  } else if (node && typeof node === 'object') {
    for (const [key, value] of Object.entries(node)) {
      if (key === 'format' && typeof value === 'string') found.add(value);
      else collectFormats(value, found);
    }
  }
  return found;
}

function build(): Ajv2020 {
  // Strict: an unknown keyword in the published schema is a bug, not something to skip.
  const ajv = new Ajv2020({ strict: true, allErrors: true });
  for (const format of collectFormats(envelopeSchema)) ajv.addFormat(format, true);
  ajv.addSchema(envelopeSchema, 'desktop-v2');
  return ajv;
}

const ajv = build();
const validateEnvelopeSchema = ajv.getSchema('desktop-v2') as ValidateFunction;
const validateOperatorCommandSchema = ajv.getSchema('desktop-v2#/$defs/OperatorCommand') as ValidateFunction;

export type Validation<T> = { ok: true; value: T } | { ok: false; errors: string[] };

function errorsOf(validate: ValidateFunction): string[] {
  return (validate.errors ?? []).map((error) => `${error.instancePath || '/'} ${error.message ?? 'is invalid'}`);
}

/**
 * Whether a value is a desktop v2 envelope by the published schema. This is
 * the structural check; Rust and the browser-specific runtime validators also
 * apply semantic rules that JSON Schema cannot express.
 */
export function validateEnvelope(value: unknown): Validation<Envelope> {
  return validateEnvelopeSchema(value)
    ? { ok: true, value: value as Envelope }
    : { ok: false, errors: errorsOf(validateEnvelopeSchema) };
}

export function assertEnvelope(value: unknown): Envelope {
  const result = validateEnvelope(value);
  if (!result.ok) throw new Error(`not a desktop v2 envelope: ${result.errors.join('; ')}`);
  return result.value;
}

/**
 * What the renderer may ask of main. Anything else is refused before it gets
 * anywhere near a native process.
 */
export function validateOperatorCommand(value: unknown): Validation<OperatorCommand> {
  return validateOperatorCommandSchema(value)
    ? { ok: true, value: value as OperatorCommand }
    : { ok: false, errors: errorsOf(validateOperatorCommandSchema) };
}
