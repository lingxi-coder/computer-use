import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  modelPlanningBackend,
  validateOwnedDocumentPlanningOptions,
  type SupervisorOptions,
} from '../src/main/supervisor.js';

type OwnedPlannerOptions = Pick<SupervisorOptions,
  'ownedDocument' | 'ownedDocumentValueAuthorization' | 'ownedDocumentSaveAuthorization'
  | 'ownedDocumentReferencePlanner' | 'ownedDocumentRootRevisit' | 'modelAuthorization'>;

const modelAuthorization = (purposes: ('s2_planning' | 's1_teacher')[]): NonNullable<SupervisorOptions['modelAuthorization']> => ({
  route: {
    provider_id: 'open_ai',
    protocol: 'open_ai_responses',
    base_url: 'https://example.invalid/v1',
    model: 'fixture-model',
    max_output_tokens: 4096,
  },
  credential: { kind: 'api_key' as const, api_key: 'owned-model-gate-test-key' },
  purposes,
});

function grants(overrides: Partial<OwnedPlannerOptions> = {}): OwnedPlannerOptions {
  return {
    ownedDocument: {} as NonNullable<SupervisorOptions['ownedDocument']>,
    ownedDocumentValueAuthorization: {} as NonNullable<SupervisorOptions['ownedDocumentValueAuthorization']>,
    ownedDocumentSaveAuthorization: {} as NonNullable<SupervisorOptions['ownedDocumentSaveAuthorization']>,
    ownedDocumentRootRevisit: true,
    ...overrides,
  };
}

test('owned Root revisit accepts diagnostic reference or real S2 planning, as separate paths', () => {
  assert.doesNotThrow(() => validateOwnedDocumentPlanningOptions(grants({
    ownedDocumentReferencePlanner: true,
  })));
  assert.doesNotThrow(() => validateOwnedDocumentPlanningOptions(grants({
    modelAuthorization: modelAuthorization(['s2_planning']),
  })));
  assert.doesNotThrow(() => validateOwnedDocumentPlanningOptions(grants({
    modelAuthorization: modelAuthorization(['s1_teacher', 's2_planning']),
  })));
  assert.doesNotThrow(() => validateOwnedDocumentPlanningOptions(grants({
    ownedDocumentRootRevisit: undefined,
    modelAuthorization: modelAuthorization(['s2_planning']),
  })));
});

test('owned production Save and revisit require S2; model plus OwnedReference is refused', () => {
  assert.equal(modelPlanningBackend(modelAuthorization(['s1_teacher'])), undefined);
  assert.throws(() => validateOwnedDocumentPlanningOptions(grants()), /one explicit planner authorization/);
  assert.throws(() => validateOwnedDocumentPlanningOptions(grants({
    modelAuthorization: modelAuthorization(['s1_teacher']),
  })), /one explicit planner authorization/);
  assert.throws(() => validateOwnedDocumentPlanningOptions(grants({
    ownedDocumentReferencePlanner: true,
    modelAuthorization: modelAuthorization(['s2_planning']),
  })), /owned Root revisit/);
});

test('Save consent alone cannot select a fake planner or bypass the value-input grant', () => {
  assert.throws(() => validateOwnedDocumentPlanningOptions(grants({
    ownedDocumentValueAuthorization: undefined,
  })), /owned Root revisit/);
  assert.throws(() => validateOwnedDocumentPlanningOptions({
    ownedDocument: {} as NonNullable<SupervisorOptions['ownedDocument']>,
    ownedDocumentValueAuthorization: {} as NonNullable<SupervisorOptions['ownedDocumentValueAuthorization']>,
    ownedDocumentSaveAuthorization: {} as NonNullable<SupervisorOptions['ownedDocumentSaveAuthorization']>,
  }), /explicit S2 planning or the isolated reference diagnostic/);
  assert.throws(() => validateOwnedDocumentPlanningOptions({
    ownedDocument: {} as NonNullable<SupervisorOptions['ownedDocument']>,
    ownedDocumentSaveAuthorization: {} as NonNullable<SupervisorOptions['ownedDocumentSaveAuthorization']>,
    modelAuthorization: modelAuthorization(['s2_planning']),
  }), /separate value-input authorization/);
  assert.throws(() => validateOwnedDocumentPlanningOptions(grants({
    ownedDocumentRootRevisit: undefined,
    modelAuthorization: modelAuthorization(['s1_teacher']),
  })), /explicit S2 planning or the isolated reference diagnostic/);
  assert.throws(() => validateOwnedDocumentPlanningOptions(grants({
    ownedDocumentSaveAuthorization: undefined,
    ownedDocumentRootRevisit: true,
  })), /owned Root revisit/);
});
