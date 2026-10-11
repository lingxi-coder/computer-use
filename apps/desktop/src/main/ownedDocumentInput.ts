import { encodeOwnedDocumentSourceBootstrap, type OwnedDocumentRegistration } from './ownedDocumentLaunch.js';

const ISSUER = Object.freeze({});
const grants = new WeakMap<OwnedDocumentValueAuthorization, { registration: OwnedDocumentRegistration; issued: number; expires: number }>();
const STARTUP_GRANT_MS = 15_000;
const saveGrants = new WeakMap<OwnedDocumentSaveAuthorization, { registration: OwnedDocumentRegistration; issued: number; expires: number }>();

/** Save needs separate Main consent; a value-input grant does not authorize it. */
export class OwnedDocumentSaveAuthorization {
  private constructor() {}

  static issue(issuer: object, registration: OwnedDocumentRegistration): OwnedDocumentSaveAuthorization {
    if (issuer !== ISSUER) throw new Error('invalid owned document save issuer');
    const proof = encodeOwnedDocumentSourceBootstrap(registration);
    proof.fill(0);
    const authorization = Object.freeze(new OwnedDocumentSaveAuthorization());
    const issued = Date.now();
    saveGrants.set(authorization, { registration, issued, expires: issued + STARTUP_GRANT_MS });
    return authorization;
  }
}

/** Call only after explicit approval to save this private owned document. */
export function authorizeOwnedDocumentSave(registration: OwnedDocumentRegistration): OwnedDocumentSaveAuthorization {
  return OwnedDocumentSaveAuthorization.issue(ISSUER, registration);
}

export function consumeOwnedDocumentSaveAuthorization(
  authorization: OwnedDocumentSaveAuthorization,
  registration: OwnedDocumentRegistration | undefined,
): void {
  const grant = saveGrants.get(authorization);
  saveGrants.delete(authorization);
  const now = Date.now();
  if (!grant || grant.registration !== registration || now < grant.issued || now >= grant.expires) {
    throw new Error('owned document save authorization is invalid, expired or already consumed');
  }
  const proof = encodeOwnedDocumentSourceBootstrap(grant.registration);
  proof.fill(0);
}

/** Main-only, one-start authorization. An owned registration alone permits observation. */
export class OwnedDocumentValueAuthorization {
  private constructor() {}

  static issue(issuer: object, registration: OwnedDocumentRegistration): OwnedDocumentValueAuthorization {
    if (issuer !== ISSUER) throw new Error('invalid owned document input issuer');
    const proof = encodeOwnedDocumentSourceBootstrap(registration);
    proof.fill(0);
    const authorization = Object.freeze(new OwnedDocumentValueAuthorization());
    const issued = Date.now();
    grants.set(authorization, { registration, issued, expires: issued + STARTUP_GRANT_MS });
    return authorization;
  }
}

/** Call only after the operator has explicitly approved input to this owned document. */
export function authorizeOwnedDocumentValueInput(registration: OwnedDocumentRegistration): OwnedDocumentValueAuthorization {
  return OwnedDocumentValueAuthorization.issue(ISSUER, registration);
}

/** Consume before any process starts; stale, forged, mismatched or reused grants fail closed. */
export function consumeOwnedDocumentValueAuthorization(
  authorization: OwnedDocumentValueAuthorization,
  registration: OwnedDocumentRegistration | undefined,
): void {
  const grant = grants.get(authorization);
  grants.delete(authorization);
  const now = Date.now();
  if (!grant || grant.registration !== registration || now < grant.issued || now >= grant.expires) {
    throw new Error('owned document input authorization is invalid, expired or already consumed');
  }
  const proof = encodeOwnedDocumentSourceBootstrap(grant.registration);
  proof.fill(0);
}
