//! Medoid-star cohort builder is deterministic and forbids similarity-chain transitive merging.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::model::projection::{CacheSequence, ComparisonDomain};

use super::{pair_metrics, PairMetrics, StructureSignature};

#[derive(Clone, Debug)]
pub struct CandidateOptions {
    pub compatibility_threshold: f64,
    pub max_dynamic_coverage_ratio: f64,
    pub min_members: usize,
    pub max_candidates_per_request: usize,
}

impl Default for CandidateOptions {
    fn default() -> Self {
        Self {
            compatibility_threshold: 0.68,
            max_dynamic_coverage_ratio: 0.35,
            min_members: 3,
            max_candidates_per_request: 128,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CandidateCohort {
    pub id: String,
    pub domain: ComparisonDomain,
    pub signature: StructureSignature,
    pub medoid: CacheSequence,
    pub members: Vec<CacheSequence>,
    pub cohesion: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSkip {
    pub domain: ComparisonDomain,
    pub member_count: usize,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct CandidateBuildResult {
    pub cohorts: Vec<CandidateCohort>,
    pub skipped: Vec<CandidateSkip>,
}

#[derive(Clone, Debug, Default)]
pub struct CandidateBuilder {
    options: CandidateOptions,
}

impl CandidateBuilder {
    pub fn new(options: CandidateOptions) -> Self {
        Self { options }
    }

    pub fn build(&self, sequences: Vec<CacheSequence>) -> CandidateBuildResult {
        let mut domains: BTreeMap<ComparisonDomain, Vec<CacheSequence>> = BTreeMap::new();
        for sequence in sequences {
            domains
                .entry(sequence.domain.clone())
                .or_default()
                .push(sequence);
        }
        let mut result = CandidateBuildResult::default();
        for (domain, mut members) in domains {
            members.sort_by(sequence_order);
            if self.options.max_candidates_per_request == 0 {
                result.skipped.push(CandidateSkip {
                    domain,
                    member_count: members.len(),
                    reason: "candidate budget is zero".to_owned(),
                });
                continue;
            }
            self.build_domain(domain, members, &mut result);
        }
        result.cohorts.sort_by(|left, right| left.id.cmp(&right.id));
        result
    }

    fn build_domain(
        &self,
        domain: ComparisonDomain,
        mut members: Vec<CacheSequence>,
        result: &mut CandidateBuildResult,
    ) {
        let seed_size = self.options.max_candidates_per_request.saturating_add(1);
        let mut medoids_examined = 0usize;
        while members.len() >= self.options.min_members {
            if medoids_examined >= self.options.max_candidates_per_request {
                result.skipped.push(CandidateSkip {
                    domain,
                    member_count: members.len(),
                    reason: format!(
                        "candidate medoid budget {} exhausted",
                        self.options.max_candidates_per_request
                    ),
                });
                return;
            }
            let seed_end = seed_size.min(members.len());
            let Some((seed_medoid_index, _, _)) = self.best_star(&members[..seed_end]) else {
                break;
            };
            medoids_examined += 1;
            let medoid = members[seed_medoid_index].clone();
            let mut compatible = Vec::new();
            for (index, member) in members.iter().enumerate() {
                let metrics = pair_metrics(&medoid, member);
                if self.accepts(metrics) {
                    compatible.push((index, metrics));
                }
            }
            if compatible.len() < self.options.min_members {
                members.remove(seed_medoid_index);
                continue;
            }
            compatible = self.validate_boundaries(&members, compatible);
            if compatible.len() < self.options.min_members {
                members.remove(seed_medoid_index);
                continue;
            }
            let score_sum: f64 = compatible
                .iter()
                .map(|(_, metrics)| metrics.compatibility)
                .sum();
            let cohesion = score_sum / compatible.len() as f64;
            let selected: Vec<_> = compatible
                .iter()
                .map(|(index, _)| members[*index].clone())
                .collect();
            let selected_set: std::collections::BTreeSet<_> =
                compatible.into_iter().map(|(index, _)| index).collect();
            members = members
                .into_iter()
                .enumerate()
                .filter_map(|(index, member)| (!selected_set.contains(&index)).then_some(member))
                .collect();
            result.cohorts.push(make_cohort(medoid, selected, cohesion));
        }
    }

    fn accepts(&self, metrics: PairMetrics) -> bool {
        metrics.compatibility + f64::EPSILON >= self.options.compatibility_threshold
            && metrics.dynamic_coverage_ratio
                <= self.options.max_dynamic_coverage_ratio + f64::EPSILON
    }

    fn validate_boundaries(
        &self,
        members: &[CacheSequence],
        compatible: Vec<(usize, PairMetrics)>,
    ) -> Vec<(usize, PairMetrics)> {
        const MAX_VALIDATORS: usize = 8;
        let mut by_boundary = compatible.clone();
        by_boundary.sort_by(|left, right| {
            left.1
                .compatibility
                .total_cmp(&right.1.compatibility)
                .then_with(|| {
                    right
                        .1
                        .dynamic_coverage_ratio
                        .total_cmp(&left.1.dynamic_coverage_ratio)
                })
                .then_with(|| left.0.cmp(&right.0))
        });
        let mut validators = Vec::new();
        for index in by_boundary.iter().take(4).map(|(index, _)| *index) {
            if !validators.contains(&index) {
                validators.push(index);
            }
        }
        for numerator in [0usize, 1, 2, 3, 4] {
            let position = numerator * compatible.len().saturating_sub(1) / 4;
            let index = compatible[position].0;
            if validators.len() < MAX_VALIDATORS && !validators.contains(&index) {
                validators.push(index);
            }
        }
        compatible
            .into_iter()
            .filter(|(member_index, _)| {
                validators.iter().all(|validator_index| {
                    member_index == validator_index
                        || self.accepts(pair_metrics(
                            &members[*member_index],
                            &members[*validator_index],
                        ))
                })
            })
            .collect()
    }

    fn best_star(&self, members: &[CacheSequence]) -> Option<(usize, Vec<usize>, f64)> {
        members
            .iter()
            .enumerate()
            .map(|(center, medoid)| {
                let mut compatible = Vec::new();
                let mut score_sum = 0.0;
                for (index, member) in members.iter().enumerate() {
                    let metrics = pair_metrics(medoid, member);
                    if self.accepts(metrics) {
                        compatible.push(index);
                        score_sum += metrics.compatibility;
                    }
                }
                let cohesion = score_sum / compatible.len().max(1) as f64;
                (center, compatible, cohesion)
            })
            .max_by(|left, right| {
                left.1
                    .len()
                    .cmp(&right.1.len())
                    .then_with(|| left.2.total_cmp(&right.2))
                    .then_with(|| sequence_order(&members[right.0], &members[left.0]))
            })
    }
}

fn sequence_order(left: &CacheSequence, right: &CacheSequence) -> std::cmp::Ordering {
    left.request_id.cmp(&right.request_id).then_with(|| {
        let left_content: Vec<_> = left.units.iter().map(|unit| &unit.content).collect();
        let right_content: Vec<_> = right.units.iter().map(|unit| &unit.content).collect();
        left_content.cmp(&right_content)
    })
}

fn make_cohort(
    medoid: CacheSequence,
    members: Vec<CacheSequence>,
    cohesion: f64,
) -> CandidateCohort {
    let signature = StructureSignature::of(&medoid);
    let mut digest = Sha256::new();
    digest.update(medoid.domain.time_window_key.to_be_bytes());
    digest.update([0]);
    digest.update(medoid.domain.endpoint_key.as_bytes());
    digest.update([0]);
    digest.update(medoid.domain.dialect.as_bytes());
    digest.update([0]);
    digest.update(medoid.domain.model.as_bytes());
    digest.update([0]);
    update_optional(&mut digest, medoid.domain.model_deployment_key.as_deref());
    digest.update(medoid.domain.context_schema_key.as_bytes());
    digest.update([0]);
    update_optional(&mut digest, medoid.domain.agent_key.as_deref());
    update_optional(&mut digest, medoid.domain.kv_namespace.as_deref());
    digest.update(medoid.domain.adapter_revision.as_bytes());
    digest.update([0]);
    digest.update(signature.0.as_bytes());
    for member in &members {
        digest.update(member.request_id.as_bytes());
        digest.update([0]);
    }
    CandidateCohort {
        id: hex::encode(digest.finalize()),
        domain: medoid.domain.clone(),
        signature,
        medoid,
        members,
        cohesion,
    }
}

fn update_optional(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.as_bytes());
        }
        None => digest.update([0]),
    }
    digest.update([0]);
}

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/discovery/candidate/cohort.rs"]
mod tests;
