use crate::{
    AgentType, BranchSpec, BudgetSpec, IsolationMode, LeafSpec, LoopUntilSpec, ModelPolicy,
    PermissionSpec, PromotionPolicy, PromotionStrategy, SequenceSpec, TaskMode, TeacherReviewSpec,
    WorkflowNode,
};

/// Declarative topology constructors for compiling agent ensembles into the
/// existing WhaleFlow IR.
pub struct WorkflowTopology;

impl WorkflowTopology {
    pub fn fan_out(id: impl Into<String>, leaves: Vec<LeafSpec>) -> WorkflowNode {
        WorkflowNode::BranchSet(BranchSpec {
            id: id.into(),
            description: None,
            parallel: true,
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
            children: leaves.into_iter().map(WorkflowNode::Leaf).collect(),
        })
    }

    pub fn pipeline(id: impl Into<String>, leaves: Vec<LeafSpec>) -> WorkflowNode {
        let mut prior_id = None;
        let children = leaves
            .into_iter()
            .map(|mut leaf| {
                if let Some(dependency) = prior_id.as_deref() {
                    add_dependency(&mut leaf, dependency);
                }
                prior_id = Some(leaf.id.clone());
                WorkflowNode::Leaf(leaf)
            })
            .collect();

        WorkflowNode::Sequence(SequenceSpec {
            id: id.into(),
            children,
        })
    }

    pub fn diamond(
        id: impl Into<String>,
        scouts: Vec<LeafSpec>,
        mut integrator: LeafSpec,
        verifiers: Vec<LeafSpec>,
    ) -> WorkflowNode {
        let id = id.into();
        let scout_ids = leaf_ids(&scouts);
        add_dependencies(&mut integrator, scout_ids.iter());
        let integrator_id = integrator.id.clone();
        let verifiers = verifiers
            .into_iter()
            .map(|mut verifier| {
                add_dependency(&mut verifier, &integrator_id);
                verifier
            })
            .collect();

        WorkflowNode::Sequence(SequenceSpec {
            id: id.clone(),
            children: vec![
                Self::fan_out(format!("{id}-scouts"), scouts),
                WorkflowNode::Leaf(integrator),
                Self::fan_out(format!("{id}-verifiers"), verifiers),
            ],
        })
    }

    pub fn speculative(
        id: impl Into<String>,
        candidates: Vec<LeafSpec>,
        mut verifier: LeafSpec,
    ) -> WorkflowNode {
        let id = id.into();
        let candidate_ids = leaf_ids(&candidates);
        add_dependencies(&mut verifier, candidate_ids.iter());
        let verifier_id = verifier.id.clone();

        WorkflowNode::Sequence(SequenceSpec {
            id: id.clone(),
            children: vec![
                Self::fan_out(format!("{id}-candidates"), candidates),
                WorkflowNode::Leaf(verifier),
                WorkflowNode::TeacherReview(TeacherReviewSpec {
                    id: format!("{id}-selection"),
                    candidates: vec![verifier_id],
                    promotion_policy: PromotionPolicy {
                        strategy: PromotionStrategy::TeacherSelected,
                        require_teacher_review: true,
                        ..PromotionPolicy::default()
                    },
                }),
            ],
        })
    }

    pub fn critique_loop(
        id: impl Into<String>,
        condition: impl Into<String>,
        max_iterations: u32,
        mut implementer: LeafSpec,
        mut reviewer: LeafSpec,
        mut fixer: LeafSpec,
    ) -> WorkflowNode {
        let implementer_id = implementer.id.clone();
        let reviewer_id = reviewer.id.clone();
        let fixer_id = fixer.id.clone();
        add_dependency(&mut implementer, &fixer_id);
        add_dependency(&mut reviewer, &implementer_id);
        add_dependency(&mut fixer, &reviewer_id);
        let id = id.into();

        WorkflowNode::LoopUntil(LoopUntilSpec {
            id: id.clone(),
            condition: condition.into(),
            max_iterations: Some(max_iterations.max(1)),
            children: vec![WorkflowNode::Sequence(SequenceSpec {
                id: format!("{id}-round"),
                children: vec![
                    WorkflowNode::Leaf(implementer),
                    WorkflowNode::Leaf(reviewer),
                    WorkflowNode::Leaf(fixer),
                ],
            })],
        })
    }

    pub fn waterfall(id: impl Into<String>, waves: Vec<Vec<LeafSpec>>) -> WorkflowNode {
        let id = id.into();
        let mut prior_wave_ids: Vec<String> = Vec::new();
        let children = waves
            .into_iter()
            .enumerate()
            .map(|(index, wave)| {
                let mut current_wave_ids = Vec::new();
                let leaves = wave
                    .into_iter()
                    .map(|mut leaf| {
                        current_wave_ids.push(leaf.id.clone());
                        add_dependencies(&mut leaf, prior_wave_ids.iter());
                        leaf
                    })
                    .collect();
                prior_wave_ids = current_wave_ids;
                Self::fan_out(format!("{id}-wave-{}", index + 1), leaves)
            })
            .collect();

        WorkflowNode::Sequence(SequenceSpec { id, children })
    }

    pub fn leaf(id: impl Into<String>, prompt: impl Into<String>) -> LeafSpec {
        LeafSpec {
            id: id.into(),
            prompt: prompt.into(),
            agent_type: AgentType::General,
            mode: TaskMode::ReadOnly,
            isolation: IsolationMode::Shared,
            file_scope: Vec::new(),
            depends_on_results: Vec::new(),
            budget: BudgetSpec::default(),
            permissions: PermissionSpec::default(),
            model_policy: ModelPolicy::default(),
        }
    }
}

fn leaf_ids(leaves: &[LeafSpec]) -> Vec<String> {
    leaves.iter().map(|leaf| leaf.id.clone()).collect()
}

fn add_dependencies<'a>(leaf: &mut LeafSpec, dependencies: impl IntoIterator<Item = &'a String>) {
    for dependency in dependencies {
        add_dependency(leaf, dependency);
    }
}

fn add_dependency(leaf: &mut LeafSpec, dependency: &str) {
    if !leaf
        .depends_on_results
        .iter()
        .any(|existing| existing == dependency)
    {
        leaf.depends_on_results.push(dependency.to_string());
    }
}
