import type { DecisionRequest } from '@computer-use/desktop-api';

export type S1QualityStratum = 'navigation' | 'disambiguation' | 'recovery' | 'bilingual';

export interface S1QualityCase {
  readonly case_id: string;
  readonly stratum: S1QualityStratum;
  readonly request: DecisionRequest;
}

export interface S1QualityReferenceLabel {
  readonly case_id: string;
  readonly expected_candidate_id: string;
  readonly rationale: string;
}

export interface S1QualityCorpus {
  readonly version: 1;
  readonly model_cases: readonly S1QualityCase[];
  readonly reference_labels: readonly S1QualityReferenceLabel[];
}

export declare const S1_QUALITY_CORPUS_VERSION: 1;
export declare const S1_QUALITY_CASE_COUNT: 20;
export declare const S1_QUALITY_STRATA: readonly S1QualityStratum[];
export declare const S1_QUALITY_CORPUS: S1QualityCorpus;

export declare function projectLayaFeatureInput(request: DecisionRequest): {
  readonly state: DecisionRequest['state'];
  readonly question: {
    readonly t: 'choice';
    readonly ins: string;
    readonly crit: Readonly<Record<string, string>>;
  };
};

export declare function fixedOptionPermutations(optionCount: number): readonly (readonly number[])[];
export declare function requestWithOptionOrder(request: DecisionRequest, order: readonly number[]): DecisionRequest;
