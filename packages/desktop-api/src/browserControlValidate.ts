import Ajv2020 from 'ajv/dist/2020.js';
import type { ValidateFunction } from 'ajv';

import type {
  BrowserControlAuthority,
  BrowserControlBootstrap,
  BrowserControlHello,
  BrowserControlOutcome,
  BrowserControlReady,
  BrowserControlReply,
  BrowserControlSubmit,
  BrowserControlToken,
} from './generated/browser.js';
import { browserWireSchema } from './generated/browser-schema.js';
import type { BrowserActionIntent, BrowserExecutionObservation } from './generated/envelope.js';
import type { Validation } from './validate.js';
import {
  validateBrowserActionIntent,
  validateBrowserExecutionObservation,
  validateBrowserReadScope,
} from './browserValidate.js';

const SAFE_INTEGER = Number.MAX_SAFE_INTEGER;
const MAX_BOOTSTRAP_BYTES = 16 * 1024;
const MAX_SUBMIT_BYTES = 64 * 1024;
const MAX_REPLY_BYTES = 4 * 1024;
const MAX_CONTROL_ID_BYTES = 256;
const MAX_REPLY_GRACE_MS = 500;
const encoder = new TextEncoder();
const controls = /[\u0000-\u001f\u007f-\u009f]/u;
const canonicalUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function byteLength(value: string): number { return encoder.encode(value).byteLength; }

function noUnpairedSurrogates(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) return false;
  }
  return true;
}

function boundedText(value: unknown, maximum: number, allowBlank = false): value is string {
  return typeof value === 'string'
    && (allowBlank || value.trim().length > 0)
    && byteLength(value) <= maximum
    && noUnpairedSurrogates(value)
    && !controls.test(value);
}

function positiveSafe(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 && value <= SAFE_INTEGER;
}

function nonnegativeSafe(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value <= SAFE_INTEGER;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
  }
  const json = JSON.stringify(value);
  if (json === undefined) throw new Error('browser control value is not JSON encodable');
  return json;
}

function sameJson(left: unknown, right: unknown): boolean {
  return canonical(left) === canonical(right);
}

function serializedBytes(value: unknown): number {
  const json = JSON.stringify(value);
  return json === undefined ? Number.POSITIVE_INFINITY : byteLength(json);
}

function buildAjv(): Ajv2020 {
  const ajv = new Ajv2020({ strict: true, allErrors: true });
  const formats = new Set<string>();
  const collectFormats = (node: unknown): void => {
    if (Array.isArray(node)) node.forEach(collectFormats);
    else if (isRecord(node)) {
      for (const [key, child] of Object.entries(node)) {
        if (key === 'format' && typeof child === 'string') formats.add(child);
        else collectFormats(child);
      }
    }
  };
  collectFormats(browserWireSchema);
  for (const format of formats) ajv.addFormat(format, true);
  ajv.addSchema(browserWireSchema, 'browser-v1');
  return ajv;
}

const ajv = buildAjv();
function schemaValidator(name: string): ValidateFunction {
  const validator = ajv.getSchema(`browser-v1#/$defs/${name}`);
  if (!validator) throw new Error(`generated browser schema is missing ${name}`);
  return validator;
}

const validateBootstrapSchema = schemaValidator('BrowserControlBootstrap');
const validateHelloSchema = schemaValidator('BrowserControlHello');
const validateReadySchema = schemaValidator('BrowserControlReady');
const validateAuthoritySchema = schemaValidator('BrowserControlAuthority');
const validateSubmitSchema = schemaValidator('BrowserControlSubmit');
const validateOutcomeSchema = schemaValidator('BrowserControlOutcome');
const validateReplySchema = schemaValidator('BrowserControlReply');

function schemaErrors<T>(validator: ValidateFunction): Validation<T> {
  return { ok: false, errors: (validator.errors ?? []).map((error) => `${error.instancePath || '/'} ${error.message ?? 'is invalid'}`) };
}

function invalid<T>(reason: string): Validation<T> { return { ok: false, errors: [reason] }; }

function validProfile(value: unknown): boolean {
  return isRecord(value) && Object.keys(value).length === 1 && value.kind === 'fixture_background_option';
}

function validControlToken(value: BrowserControlToken): boolean {
  // Deliberately do not include the token in any error string or diagnostic.
  return typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);
}

function sameSecret(left: string, right: string): boolean {
  let difference = left.length ^ right.length;
  const length = Math.max(left.length, right.length);
  for (let index = 0; index < length; index += 1) {
    difference |= (left.charCodeAt(index) || 0) ^ (right.charCodeAt(index) || 0);
  }
  return difference === 0;
}

function validHelloShape(hello: BrowserControlHello): boolean {
  return hello.version === 1
    && validControlToken(hello.token)
    && positiveSafe(hello.channel_generation)
    && positiveSafe(hello.session_epoch)
    && validProfile(hello.browser_page_profile)
    && typeof hello.scope_id === 'string' && canonicalUuid.test(hello.scope_id)
    && positiveSafe(hello.scope_generation);
}

function validateSubmitAuthority(
  intent: BrowserActionIntent,
  authority: BrowserControlAuthority,
  authorityEvidence: BrowserExecutionObservation,
): string | undefined {
  if (![authority.task_id, authority.attempt_id, authority.permit.id].every((value) =>
    boundedText(value, MAX_CONTROL_ID_BYTES))) {
    return 'browser control authority identifiers are invalid';
  }
  if (![authority.task_revision, authority.root_epoch, authority.permit.generation].every(positiveSafe)
    || ![authority.stop_seq, authority.command_start_ms, authority.command_end_ms,
      authority.valid_until_ms, authority.reply_deadline_ms].every(nonnegativeSafe)) {
    return 'browser control authority integers must be JS-safe';
  }
  if (authority.task_id !== intent.task_id
    || authority.task_revision !== intent.task_revision
    || authority.root_epoch !== intent.root_epoch
    || authority.attempt_id !== intent.attempt_id
    || !sameJson(authority.permit, intent.permit)) {
    return 'browser control authority does not match its intent identity';
  }

  const permitObservation = intent.evidence;
  const permitBinding = authorityEvidence.binding;
  const currentBinding = permitObservation.binding;
  const permitSequence = authorityEvidence.binding.source_sequence;
  const currentSequence = permitObservation.binding.source_sequence;
  const sameRead = permitSequence === currentSequence
    && authorityEvidence.observation_id === permitObservation.observation_id
    && authorityEvidence.binding.observed_ms === permitObservation.binding.observed_ms
    && permitBinding.read_finished_ms === currentBinding.read_finished_ms;
  const readOrderInvalid = permitSequence > currentSequence
    || (permitSequence === currentSequence && !sameRead)
    || (permitSequence < currentSequence
      && (currentBinding.observed_ms < permitBinding.read_finished_ms
        || authorityEvidence.observation_id === permitObservation.observation_id));
  if (!sameJson(authorityEvidence.scope, permitObservation.scope)
    || authorityEvidence.binding.clock_domain !== permitObservation.binding.clock_domain
    || !sameJson(permitBinding.context, currentBinding.context)
    || permitBinding.target_context_hash !== currentBinding.target_context_hash
    || permitBinding.ax_node_id !== currentBinding.ax_node_id
    || permitBinding.backend_dom_node_id !== currentBinding.backend_dom_node_id
    || permitBinding.role !== currentBinding.role
    || permitBinding.name !== currentBinding.name
    || permitBinding.disabled !== currentBinding.disabled
    || permitBinding.checked !== currentBinding.checked
    || readOrderInvalid) {
    return 'browser permit evidence and execution evidence identify different targets';
  }
  const actionEnd = authority.command_start_ms + intent.duration_ms;
  const maximumReplyDeadline = authority.valid_until_ms > SAFE_INTEGER - MAX_REPLY_GRACE_MS
    ? SAFE_INTEGER : authority.valid_until_ms + MAX_REPLY_GRACE_MS;
  if (!Number.isSafeInteger(actionEnd) || actionEnd > SAFE_INTEGER
    || authority.command_start_ms < permitObservation.binding.read_finished_ms
    || authority.command_start_ms > intent.latest_start_ms
    || authority.command_end_ms !== actionEnd
    || authority.valid_until_ms < authority.command_end_ms
    || authority.valid_until_ms > permitObservation.valid_until_ms
    || authority.valid_until_ms > authorityEvidence.valid_until_ms
    || authority.reply_deadline_ms < authority.command_end_ms
    || authority.reply_deadline_ms > maximumReplyDeadline
    || intent.action.checked === permitBinding.checked) {
    return 'browser control authority has an invalid bounded command window';
  }
  return undefined;
}

/** Validates the secret-bearing bootstrap against the single fixed fixture scope. */
export function validateBrowserControlBootstrap(value: unknown): Validation<BrowserControlBootstrap> {
  if (!validateBootstrapSchema(value)) return schemaErrors(validateBootstrapSchema);
  const bootstrap = value as BrowserControlBootstrap;
  const scope = validateBrowserReadScope(bootstrap.scope);
  if (!scope.ok) return invalid('browser control bootstrap scope is invalid');
  if (bootstrap.version !== 1 || !validControlToken(bootstrap.token)
    || !positiveSafe(bootstrap.channel_generation) || !validProfile(bootstrap.browser_page_profile)
    || !bootstrap.scope.context.fixture_file_url.endsWith('/fixtures/headless/readonly-page.html')) {
    return invalid('browser control bootstrap does not match the fixed fixture contract');
  }
  if (serializedBytes(bootstrap) > MAX_BOOTSTRAP_BYTES) return invalid('browser control bootstrap exceeds 16 KiB');
  return { ok: true, value: bootstrap };
}

/** Requires the Main-created bootstrap so Hello cannot select another session or target. */
export function validateBrowserControlHello(
  value: unknown,
  bootstrap: BrowserControlBootstrap,
): Validation<BrowserControlHello> {
  const checkedBootstrap = validateBrowserControlBootstrap(bootstrap);
  if (!checkedBootstrap.ok) return invalid('browser control bootstrap is invalid');
  if (!validateHelloSchema(value)) return schemaErrors(validateHelloSchema);
  const hello = value as BrowserControlHello;
  if (!validHelloShape(hello)
    || !sameSecret(hello.token, checkedBootstrap.value.token)
    || hello.channel_generation !== checkedBootstrap.value.channel_generation
    || hello.session_epoch !== checkedBootstrap.value.scope.context.session_epoch
    || !sameJson(hello.browser_page_profile, checkedBootstrap.value.browser_page_profile)
    || hello.scope_id !== checkedBootstrap.value.scope.scope_id
    || hello.scope_generation !== checkedBootstrap.value.scope.generation) {
    return invalid('browser control hello does not match its private bootstrap');
  }
  if (serializedBytes(hello) > MAX_BOOTSTRAP_BYTES) return invalid('browser control hello exceeds 16 KiB');
  return { ok: true, value: hello };
}

/** Verifies Main's ready frame names the same Hello and starts at sequence one. */
export function validateBrowserControlReady(
  value: unknown,
  hello?: BrowserControlHello,
): Validation<BrowserControlReady> {
  if (!validateReadySchema(value)) return schemaErrors(validateReadySchema);
  const ready = value as BrowserControlReady;
  if (ready.version !== 1
    || !positiveSafe(ready.channel_generation)
    || !positiveSafe(ready.session_epoch)
    || typeof ready.scope_id !== 'string' || !canonicalUuid.test(ready.scope_id)
    || !positiveSafe(ready.scope_generation)
    || ready.next_sequence !== 1
    || (hello !== undefined && (!validateHelloSchema(hello)
      || !validHelloShape(hello)
      || serializedBytes(hello) > MAX_BOOTSTRAP_BYTES
      || ready.channel_generation !== hello.channel_generation
      || ready.session_epoch !== hello.session_epoch
      || ready.scope_id !== hello.scope_id
      || ready.scope_generation !== hello.scope_generation))) {
    return invalid('browser control ready does not match its hello or first sequence');
  }
  return { ok: true, value: ready };
}

/** Validates authority as a standalone DTO; it never mints or grants a permit. */
export function validateBrowserControlAuthority(value: unknown): Validation<BrowserControlAuthority> {
  if (!validateAuthoritySchema(value)) return schemaErrors(validateAuthoritySchema);
  const authority = value as BrowserControlAuthority;
  if (![authority.task_id, authority.attempt_id, authority.permit.id].every((text) =>
    boundedText(text, MAX_CONTROL_ID_BYTES))) return invalid('browser control authority identifiers are invalid');
  if (![authority.task_revision, authority.root_epoch, authority.permit.generation].every(positiveSafe)
    || ![authority.stop_seq, authority.command_start_ms, authority.command_end_ms,
      authority.valid_until_ms, authority.reply_deadline_ms].every(nonnegativeSafe)) {
    return invalid('browser control authority integers must be JS-safe');
  }
  return { ok: true, value: authority };
}

/** Validates an incoming private Device submit, including its separate original authority evidence. */
export async function validateBrowserControlSubmit(value: unknown): Promise<Validation<BrowserControlSubmit>> {
  if (!validateSubmitSchema(value)) return schemaErrors(validateSubmitSchema);
  const submit = value as BrowserControlSubmit;
  if (submit.version !== 1
    || typeof submit.request_id !== 'string' || !canonicalUuid.test(submit.request_id)
    || !positiveSafe(submit.channel_generation)
    || !positiveSafe(submit.session_epoch)
    || typeof submit.scope_id !== 'string' || !canonicalUuid.test(submit.scope_id)
    || !positiveSafe(submit.scope_generation)
    || !positiveSafe(submit.sequence)) {
    return invalid('browser control submit version, identity, or sequence is invalid');
  }
  const intentValidation = await validateBrowserActionIntent(submit.intent);
  if (!intentValidation.ok) return invalid('browser control submit intent is invalid');
  const authorityEvidenceValidation = await validateBrowserExecutionObservation(submit.authority_evidence);
  if (!authorityEvidenceValidation.ok) return invalid('browser control authority evidence is invalid');
  const authorityValidation = validateBrowserControlAuthority(submit.authority);
  if (!authorityValidation.ok) return invalid('browser control authority is invalid');
  const intent = intentValidation.value;
  const authorityEvidence = authorityEvidenceValidation.value;
  const authorityError = validateSubmitAuthority(intent, authorityValidation.value, authorityEvidence);
  if (authorityError) return invalid(authorityError);
  if (submit.session_epoch !== intent.session_epoch
    || submit.scope_id !== intent.evidence.scope.scope_id
    || submit.scope_generation !== intent.evidence.scope.generation
    || intent.evidence.scope.context.session_epoch !== submit.session_epoch) {
    return invalid('browser control submit differs from its intent session or scope');
  }
  if (serializedBytes(submit) > MAX_SUBMIT_BYTES) return invalid('browser control submit exceeds 64 KiB');
  return { ok: true, value: submit };
}

export function validateBrowserControlOutcome(value: unknown): Validation<BrowserControlOutcome> {
  return validateOutcomeSchema(value)
    ? { ok: true, value: value as BrowserControlOutcome }
    : schemaErrors(validateOutcomeSchema);
}

/** Validates and correlates a private Device reply to the one outstanding submit. */
export async function validateBrowserControlReply(
  value: unknown,
  submitValue: unknown,
): Promise<Validation<BrowserControlReply>> {
  const submitValidation = await validateBrowserControlSubmit(submitValue);
  if (!submitValidation.ok) return invalid('browser control submit is invalid');
  if (!validateReplySchema(value)) return schemaErrors(validateReplySchema);
  const reply = value as BrowserControlReply;
  const submit = submitValidation.value;
  const submittedAt = reply.submitted_at_ms ?? null;
  const inputEnd = reply.input_end_ms ?? null;
  const failureCode = reply.failure_code ?? null;
  if (reply.version !== 1
    || reply.request_id !== submit.request_id
    || reply.channel_generation !== submit.channel_generation
    || reply.session_epoch !== submit.session_epoch
    || reply.scope_id !== submit.scope_id
    || reply.scope_generation !== submit.scope_generation
    || reply.sequence !== submit.sequence
    || reply.intent_id !== submit.intent.intent_id
    || !canonicalUuid.test(reply.request_id)
    || !canonicalUuid.test(reply.scope_id)
    || !positiveSafe(reply.channel_generation)
    || !positiveSafe(reply.session_epoch)
    || !positiveSafe(reply.scope_generation)
    || !positiveSafe(reply.sequence)
    || !boundedText(reply.intent_id, MAX_CONTROL_ID_BYTES)) {
    return invalid('browser control reply does not correlate to its submit');
  }
  if (submittedAt !== null && !nonnegativeSafe(submittedAt)
    || inputEnd !== null && !nonnegativeSafe(inputEnd)) {
    return invalid('browser control reply times must be JS-safe');
  }
  if (reply.outcome === 'completed') {
    if (submittedAt === null || inputEnd === null || failureCode !== null
      || submittedAt < submit.authority.command_start_ms
      || inputEnd < submittedAt
      || inputEnd > submit.authority.command_end_ms
      || inputEnd > submit.authority.reply_deadline_ms) {
      return invalid('completed browser reply times or failure code are invalid');
    }
  } else if (reply.outcome === 'rejected') {
    if (submittedAt !== null || inputEnd !== null || failureCode === null) {
      return invalid('rejected browser reply must carry only a failure code');
    }
  } else if (reply.outcome === 'unknown') {
    if (submittedAt === null && inputEnd === null
      || submittedAt !== null && inputEnd !== null && inputEnd < submittedAt) {
      return invalid('unknown browser reply must preserve a non-reversed observed boundary');
    }
  }
  const canonicalReply: BrowserControlReply = {
    version: reply.version,
    request_id: reply.request_id,
    channel_generation: reply.channel_generation,
    session_epoch: reply.session_epoch,
    scope_id: reply.scope_id,
    scope_generation: reply.scope_generation,
    sequence: reply.sequence,
    intent_id: reply.intent_id,
    outcome: reply.outcome,
    submitted_at_ms: submittedAt,
    input_end_ms: inputEnd,
    failure_code: failureCode,
  };
  if (serializedBytes(canonicalReply) > MAX_REPLY_BYTES) return invalid('browser control reply exceeds 4 KiB');
  return { ok: true, value: reply };
}
