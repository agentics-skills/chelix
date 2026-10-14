import type { VNode } from "preact";

interface SkillAgentTagsProps {
	agents: string[];
}

export function SkillAgentTags({ agents }: SkillAgentTagsProps): VNode | null {
	if (!agents.length) return null;
	return (
		<>
			{agents.map((agent, index) => (
				<span key={`${agent}:${index}`} className="skills-agent-tag">
					{agent}
				</span>
			))}
		</>
	);
}

interface SkillAgentFilterItem {
	allowed_agents: string[];
}

interface SkillAgentFiltersProps {
	skills: SkillAgentFilterItem[];
	activeAgent: string | null;
	allActive: boolean;
	onShowAll: () => void;
	onSelectAgent: (agent: string) => void;
	onClearAgent: () => void;
}

export function SkillAgentFilters({
	skills,
	activeAgent,
	allActive,
	onShowAll,
	onSelectAgent,
	onClearAgent,
}: SkillAgentFiltersProps): VNode | null {
	const agents = [...new Set(skills.flatMap((skill) => skill.allowed_agents))].sort();
	if (!agents.length) return null;
	return (
		<div className="flex flex-wrap gap-1.5 mb-3">
			<button type="button" className={`skills-agent-pill ${allActive ? "active" : ""}`} onClick={onShowAll}>
				All ({skills.length})
			</button>
			{agents.map((agent) => {
				const count = skills.filter((skill) => skill.allowed_agents.includes(agent)).length;
				return (
					<button
						type="button"
						key={agent}
						className={`skills-agent-pill ${activeAgent === agent ? "active" : ""}`}
						onClick={() => {
							if (activeAgent === agent) onClearAgent();
							else onSelectAgent(agent);
						}}
					>
						{agent} ({count})
					</button>
				);
			})}
		</div>
	);
}
