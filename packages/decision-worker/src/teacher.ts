import type { Candidate, DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';

import { unavailable, type DecisionBackend } from './worker.js';

const CONTROL_SKILLS = new Set(['observe_more', 'request_planning', 'stop_or_yield']);

function words(text: string): string[] {
  return text
    .toLowerCase()
    .split(/\s+/)
    .map((word) => word.replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, ''))
    .filter((word) => word.length > 0);
}

function overlap(description: string, goal: string): number {
  const wanted = new Set(words(goal));
  return words(description).filter((word) => wanted.has(word)).length;
}

export interface ScriptedTeacherOptions {
  /** Prefer the candidate whose description contains this word. */
  preferWord?: string;
  /** How long it takes to answer, for tests of cancellation and late results. */
  delayMs?: number;
  /** Answer as a backend that is not there. */
  unavailable?: boolean;
  backend?: string;
}

/**
 * A deterministic stand-in for the teacher model that fills the S1 slot. It is
 * not a model: it picks among the candidates the host already allowed, by fixed
 * preferences, so every decision is reproducible. It mirrors the Rust
 * ScriptedTeacher the simulated world uses.
 */
export class ScriptedTeacher implements DecisionBackend {
  readonly name: string;

  constructor(private readonly options: ScriptedTeacherOptions = {}) {
    this.name = options.backend ?? 'teacher-scripted';
  }

  async decide(request: DecisionRequest): Promise<DecisionProposal> {
    if (this.options.delayMs) await new Promise((done) => setTimeout(done, this.options.delayMs));
    if (this.options.unavailable) return unavailable(request, this.name);

    const executable = request.state.candidates.filter((candidate) => candidate.precondition.state === 'executable');
    const goal = request.state.goal_summary;
    const prefer = this.options.preferWord?.toLowerCase();
    const preferred = prefer
      ? executable.find((candidate) => candidate.description.toLowerCase().includes(prefer))
      : undefined;
    const best = (): Candidate | undefined => {
      let winner: Candidate | undefined;
      let score = -1;
      for (const candidate of executable.filter((c) => !CONTROL_SKILLS.has(c.skill))) {
        const value = overlap(candidate.description, goal);
        // Ties go to the smaller candidate id, as the Rust stand-in does.
        if (value > score || (value === score && winner && candidate.candidate_id < winner.candidate_id)) {
          winner = candidate;
          score = value;
        }
      }
      return winner;
    };
    const picked = preferred ?? best() ?? executable[0];

    return {
      decision_id: request.decision_id,
      request_generation: request.request_generation,
      candidate_set_digest: request.state.candidate_set_digest,
      backend: this.name,
      source: 'teacher',
      model_revision: 'scripted-1',
      answers: request.questions.map((question) => ({
        question_id: question.id,
        selected: picked?.candidate_id ?? null,
        confidence_milli: 900,
      })),
      choice: picked
        ? { action: 'start_skill', candidate: picked.candidate_id }
        : { action: 'observe_more', facts: ['a fresh observation'] },
      failure: null,
      truncated: false,
      queue_ms: 0,
      inference_ms: 1,
    };
  }
}
