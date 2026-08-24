import { commands, isTauri } from '../../tauri';
import { prefGet, prefSet } from '../../lib/prefs';
import {
  EMPTY_COLLABORATION_SNAPSHOT,
  type CollaborationBootstrapPlanDto,
  type CollaborationLoadResult,
  type CollaborationMemberLaunchPlanDto,
  type CollaborationPersistence,
  type CollaborationSnapshotDto,
  type CollaborationTeamDto,
  type GrokSessionOptionDto,
} from './types';

const DRAFT_KEY = 'collaboration-settings-draft-v1';

function isSnapshot(value: unknown): value is CollaborationSnapshotDto {
  if (!value || typeof value !== 'object') return false;
  const record = value as Record<string, unknown>;
  return typeof record.enabled === 'boolean' && Array.isArray(record.teams);
}

function readDraft(): CollaborationSnapshotDto {
  try {
    const raw = prefGet(DRAFT_KEY);
    if (!raw) return { ...EMPTY_COLLABORATION_SNAPSHOT };
    const parsed: unknown = JSON.parse(raw);
    if (!isSnapshot(parsed)) return { ...EMPTY_COLLABORATION_SNAPSHOT };
    // A local draft is never an active collaboration runtime. Keep the switch
    // visibly off until the native service confirms that it is enabled.
    return { ...parsed, enabled: false };
  } catch {
    return { ...EMPTY_COLLABORATION_SNAPSHOT };
  }
}

export function persistCollaborationDraft(snapshot: CollaborationSnapshotDto): void {
  prefSet(DRAFT_KEY, JSON.stringify({ ...snapshot, enabled: false }));
}

export async function loadCollaborationSettings(): Promise<CollaborationLoadResult> {
  if (isTauri) {
    try {
      return {
        snapshot: await commands.collaborationGetSnapshot(),
        persistence: 'backend',
      };
    } catch (error) {
      return {
        snapshot: readDraft(),
        persistence: 'draft',
        warning: String(error),
      };
    }
  }

  return { snapshot: readDraft(), persistence: 'draft' };
}

export async function loadGrokSessionOptions(
  persistence: CollaborationPersistence,
): Promise<GrokSessionOptionDto[]> {
  if (persistence === 'backend') {
    return commands.collaborationListGrokSessions();
  }
  if (!isTauri) return [];

  try {
    const sessions = await commands.getNativeHistory(true);
    const unique = new Map<string, GrokSessionOptionDto>();
    for (const session of sessions) {
      if (session.tool.toLowerCase() !== 'grok') continue;
      const nativeSessionId = session.session_token || session.id;
      if (!nativeSessionId || unique.has(nativeSessionId)) continue;
      unique.set(nativeSessionId, {
        nativeSessionId,
        label: session.name || nativeSessionId,
        workspace: session.cwd || '',
        state: 'saved',
      });
    }
    return [...unique.values()];
  } catch {
    return [];
  }
}

export async function setCollaborationEnabled(
  enabled: boolean,
  persistence: CollaborationPersistence,
): Promise<void> {
  if (persistence !== 'backend') {
    throw new Error('collaboration backend unavailable');
  }
  await commands.collaborationSetEnabled(enabled);
}

export async function saveCollaborationTeam(
  team: CollaborationTeamDto,
  snapshot: CollaborationSnapshotDto,
  persistence: CollaborationPersistence,
): Promise<CollaborationTeamDto> {
  if (persistence === 'backend') return commands.collaborationSaveTeam(team);
  const teams = snapshot.teams.some(item => item.id === team.id)
    ? snapshot.teams.map(item => item.id === team.id ? team : item)
    : [...snapshot.teams, team];
  persistCollaborationDraft({ ...snapshot, teams });
  return team;
}

export async function setCollaborationTeamPaused(
  team: CollaborationTeamDto,
  paused: boolean,
  snapshot: CollaborationSnapshotDto,
  persistence: CollaborationPersistence,
): Promise<CollaborationTeamDto> {
  if (persistence === 'backend') {
    return commands.collaborationSetTeamPaused(team.id, paused);
  }
  const updated = { ...team, paused };
  persistCollaborationDraft({
    ...snapshot,
    teams: snapshot.teams.map(item => item.id === team.id ? updated : item),
  });
  return updated;
}

export async function archiveCollaborationTeam(
  team: CollaborationTeamDto,
  snapshot: CollaborationSnapshotDto,
  persistence: CollaborationPersistence,
): Promise<void> {
  if (persistence === 'backend') {
    await commands.collaborationArchiveTeam(team.id);
    return;
  }
  persistCollaborationDraft({
    ...snapshot,
    teams: snapshot.teams.map(item => item.id === team.id
      ? { ...item, archived: true, paused: true }
      : item),
  });
}

export async function getCollaborationMemberLaunchPlan(
  teamId: string,
  memberId: string,
  expectedRevision: number,
  persistence: CollaborationPersistence,
): Promise<CollaborationMemberLaunchPlanDto> {
  if (persistence !== 'backend') throw new Error('collaboration backend unavailable');
  return commands.collaborationGetMemberLaunchPlan(teamId, memberId, expectedRevision);
}

export async function beginCollaborationBootstrap(
  teamId: string,
  memberId: string,
  terminalSessionId: string,
  expectedGeneration: number,
  persistence: CollaborationPersistence,
): Promise<CollaborationBootstrapPlanDto> {
  if (persistence !== 'backend') throw new Error('collaboration backend unavailable');
  return commands.collaborationBeginBootstrap(
    teamId,
    memberId,
    terminalSessionId,
    expectedGeneration,
  );
}
