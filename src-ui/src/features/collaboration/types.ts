export const COLLABORATION_MAX_WORKERS = 3;

export type CollaborationMemberRole = 'leader' | 'worker';
export type CollaborationMemberStatus =
  | 'unbound'
  | 'connecting'
  | 'ready'
  | 'busy'
  | 'waiting_user'
  | 'offline'
  | 'error';

export interface CollaborationMemberDto {
  id: string;
  alias: string;
  displayName: string;
  role: CollaborationMemberRole;
  avatarId: string;
  nativeSessionId: string;
  status: CollaborationMemberStatus;
}

export type CollaborationReportOutcome = 'completed' | 'failed';

export interface CollaborationReportDeliveryDto {
  id: string;
  workerMemberId: string;
  outcome: CollaborationReportOutcome;
  deliveredAt: number;
}

export interface CollaborationTeamDto {
  id: string;
  name: string;
  workspace: string;
  provider: 'grok-build';
  leader: CollaborationMemberDto;
  workers: CollaborationMemberDto[];
  paused: boolean;
  archived: boolean;
  revision?: number;
  pendingTasks?: number;
  recentReports?: CollaborationReportDeliveryDto[];
}

export interface CollaborationSnapshotDto {
  enabled: boolean;
  teams: CollaborationTeamDto[];
}

export type GrokSessionState = 'saved' | 'live' | 'ready' | 'busy' | 'waiting_user' | 'offline';

export interface GrokSessionOptionDto {
  nativeSessionId: string;
  label: string;
  workspace: string;
  state: GrokSessionState;
}

export type CollaborationLaunchPlanStatus =
  | 'already_collaboration_active'
  | 'ordinary_live_collision'
  | 'resume_allowed'
  | 'blocked';

export interface CollaborationMemberLaunchPlanDto {
  status: CollaborationLaunchPlanStatus;
  reasonCode?: string;
  teamId: string;
  memberId: string;
  memberAlias: string;
  memberDisplayName: string;
  terminalTitle: string;
  workspace: string;
  nativeSessionId: string;
  revision: number;
  terminalSessionId?: string;
  runtimeGeneration?: number;
}

export type CollaborationBootstrapStatus = 'already_ready' | 'prompt_required';

export interface CollaborationBootstrapPlanDto {
  status: CollaborationBootstrapStatus;
  attemptId?: string;
  prompt?: string;
  terminalSessionId: string;
  runtimeGeneration: number;
}

export type CollaborationPersistence = 'backend' | 'draft';

export interface CollaborationLoadResult {
  snapshot: CollaborationSnapshotDto;
  persistence: CollaborationPersistence;
  warning?: string;
}

export const EMPTY_COLLABORATION_SNAPSHOT: CollaborationSnapshotDto = {
  enabled: false,
  teams: [],
};

function localId(prefix: string): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return `${prefix}-${crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 9)}`;
}

export function createCollaborationMember(
  role: CollaborationMemberRole,
  workerIndex = 0,
): CollaborationMemberDto {
  const isLeader = role === 'leader';
  return {
    id: localId('member'),
    alias: isLeader ? 'main' : `worker-${String.fromCharCode(97 + workerIndex)}`,
    displayName: isLeader ? 'Main' : `Worker ${workerIndex + 1}`,
    role,
    avatarId: isLeader ? 'cedar' : ['moss', 'ember', 'luna'][workerIndex % 3],
    nativeSessionId: '',
    status: 'unbound',
  };
}

export function createCollaborationTeam(index: number): CollaborationTeamDto {
  return {
    id: localId('team'),
    name: `Grok Team ${index}`,
    workspace: '',
    provider: 'grok-build',
    leader: createCollaborationMember('leader'),
    workers: [createCollaborationMember('worker', 0)],
    paused: true,
    archived: false,
  };
}

export function collaborationMembers(team: CollaborationTeamDto): CollaborationMemberDto[] {
  return [team.leader, ...team.workers];
}
