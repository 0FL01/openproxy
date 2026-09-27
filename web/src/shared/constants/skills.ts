// Agent Skills metadata — single source of truth for /dashboard/skills page.
// Each skill = 1 raw GitHub URL the user copies and pastes to any AI agent.

const REPO = "0FL01/openproxy";
const BRANCH = "main";
const SKILL_PATH = ".agents/skills";

export const SKILLS_REPO_URL = `https://github.com/${REPO}`;
export const SKILLS_RAW_BASE = `https://raw.githubusercontent.com/${REPO}/refs/heads/${BRANCH}/${SKILL_PATH}`;
export const SKILLS_BLOB_BASE = `https://github.com/${REPO}/blob/${BRANCH}/${SKILL_PATH}`;

export interface Skill {
  id: string;
  name: string;
  description: string;
  endpoint: string | null;
  icon: string;
  isEntry?: boolean;
}

export const SKILLS: Skill[] = [
  {
    id: "openproxy",
    name: "OpenProxy (Entry)",
    description: "Build this fork from source or Compose, initialize the server, configure providers, and connect an AI client.",
    endpoint: null,
    icon: "hub",
    isEntry: true,
  },
];

export function getSkillRawUrl(id: string): string {
  return `${SKILLS_RAW_BASE}/${id}/SKILL.md`;
}

export function getSkillBlobUrl(id: string): string {
  return `${SKILLS_BLOB_BASE}/${id}/SKILL.md`;
}
