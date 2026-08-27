import { useEffect, useMemo, useState } from 'react';
import type { I18nKey } from '../../i18n/en';
import { useT } from '../../i18n/useT';
import { clipboardWrite } from '../../lib/clipboard';
import { getTabActions } from '../../lib/tab-actions';
import { useAppDispatch, useAppState } from '../../store/app-state';
import {
  archiveCollaborationTeam,
  beginCollaborationBootstrap,
  getCollaborationMemberLaunchPlan,
  loadCollaborationSettings,
  loadGrokSessionOptions,
  saveCollaborationTeam,
  setCollaborationEnabled,
  setCollaborationTeamPaused,
} from './api';
import { PixelAvatar } from './PixelAvatar';
import { PIXEL_AVATARS } from './pixel-avatars';
import {
  COLLABORATION_MAX_WORKERS,
  EMPTY_COLLABORATION_SNAPSHOT,
  collaborationMembers,
  createCollaborationMember,
  createCollaborationTeam,
  type CollaborationMemberDto,
  type CollaborationBootstrapPlanDto,
  type CollaborationMemberStatus,
  type CollaborationPersistence,
  type CollaborationSnapshotDto,
  type CollaborationTeamDto,
  type GrokSessionOptionDto,
} from './types';
import { CollaborationOffice } from './CollaborationOffice';
import './CollaborationSettings.css';

const ALIAS_RE = /^[a-z][a-z0-9-]{1,31}$/;

const STATUS_KEY: Record<CollaborationMemberStatus, I18nKey> = {
  unbound: 'collab.status.unbound',
  connecting: 'collab.status.connecting',
  ready: 'collab.status.ready',
  busy: 'collab.status.busy',
  waiting_user: 'collab.status.waiting_user',
  offline: 'collab.status.offline',
  error: 'collab.status.error',
};

function validateTeam(team: CollaborationTeamDto): I18nKey | null {
  if (!team.name.trim()) return 'collab.error.team_name';
  if (!team.workspace.trim()) return 'collab.error.workspace';
  if (team.workers.length < 1 || team.workers.length > COLLABORATION_MAX_WORKERS) return 'collab.error.limit';
  const members = collaborationMembers(team);
  const aliases = new Set<string>();
  const sessions = new Set<string>();
  for (const member of members) {
    if (!member.displayName.trim()) return 'collab.error.member_name';
    if (!ALIAS_RE.test(member.alias)) return 'collab.error.alias';
    if (aliases.has(member.alias)) return 'collab.error.alias_duplicate';
    aliases.add(member.alias);
    if (!member.nativeSessionId) continue;
    if (sessions.has(member.nativeSessionId)) return 'collab.error.session_duplicate';
    sessions.add(member.nativeSessionId);
  }
  return null;
}

function replaceTeam(
  snapshot: CollaborationSnapshotDto,
  team: CollaborationTeamDto,
): CollaborationSnapshotDto {
  const found = snapshot.teams.some(item => item.id === team.id);
  return {
    ...snapshot,
    teams: found
      ? snapshot.teams.map(item => item.id === team.id ? team : item)
      : [...snapshot.teams, team],
  };
}

function sameBootstrapAuthorization(
  preview: CollaborationBootstrapPlanDto,
  authorized: CollaborationBootstrapPlanDto,
): boolean {
  return authorized.status === 'prompt_required'
    && authorized.attemptId === preview.attemptId
    && authorized.prompt === preview.prompt
    && authorized.terminalSessionId === preview.terminalSessionId
    && authorized.runtimeGeneration === preview.runtimeGeneration;
}

function sessionBoundToAnotherMember(
  snapshot: CollaborationSnapshotDto,
  nativeSessionId: string,
  memberId: string,
): boolean {
  if (!nativeSessionId) return false;
  return snapshot.teams.some(team => collaborationMembers(team).some(candidate => (
    candidate.id !== memberId && candidate.nativeSessionId === nativeSessionId
  )));
}

export function CollaborationSettings({
  onDirtyChange,
}: {
  onDirtyChange?: (dirty: boolean) => void;
} = {}) {
  const t = useT();
  const { state } = useAppState();
  const dispatch = useAppDispatch();
  const [view, setView] = useState<'editor' | 'office'>('editor');
  const [snapshot, setSnapshot] = useState<CollaborationSnapshotDto>(EMPTY_COLLABORATION_SNAPSHOT);
  const [persistence, setPersistence] = useState<CollaborationPersistence>('draft');
  const [sessions, setSessions] = useState<GrokSessionOptionDto[]>([]);
  const [pendingNewSession, setPendingNewSession] = useState<{
    terminalId: string;
    teamId: string;
    memberId: string;
    nativeSessionId: string;
  } | null>(null);
  const [selectedTeamId, setSelectedTeamId] = useState<string | null>(null);
  const [selectedMemberId, setSelectedMemberId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshingOffice, setRefreshingOffice] = useState(false);
  const [saving, setSaving] = useState(false);
  const [dirtyTeamIds, setDirtyTeamIds] = useState<Set<string>>(() => new Set());
  const [errorKey, setErrorKey] = useState<I18nKey | null>(null);
  const [savedPulse, setSavedPulse] = useState(false);
  const [memberActionBusy, setMemberActionBusy] = useState(false);
  const [memberActionMessage, setMemberActionMessage] = useState('');
  const [bootstrapPlan, setBootstrapPlan] = useState<CollaborationBootstrapPlanDto | null>(null);

  useEffect(() => {
    let cancelled = false;
    void loadCollaborationSettings().then(result => {
      if (cancelled) return;
      setSnapshot(result.snapshot);
      setPersistence(result.persistence);
      const first = result.snapshot.teams.find(team => !team.archived) ?? result.snapshot.teams[0];
      setSelectedTeamId(first?.id ?? null);
      setSelectedMemberId(first?.leader.id ?? null);
      // Rendering the roster must not wait for a potentially large native
      // history scan. Session choices hydrate independently in the background.
      setLoading(false);
      void loadGrokSessionOptions(result.persistence).then(options => {
        if (!cancelled) setSessions(options);
      }).catch(() => {
        if (!cancelled) setSessions([]);
      });
    }).catch(() => {
      if (!cancelled) setErrorKey('collab.error.load');
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, []);

  const team = snapshot.teams.find(item => item.id === selectedTeamId) ?? null;
  const members = useMemo(() => team ? collaborationMembers(team) : [], [team]);
  const member = members.find(item => item.id === selectedMemberId) ?? members[0] ?? null;
  const dirty = team ? dirtyTeamIds.has(team.id) : false;
  const hasDirtyTeams = dirtyTeamIds.size > 0;
  const interactionBusy = saving || memberActionBusy || refreshingOffice;
  const canEdit = Boolean(team?.paused && !team.archived && !interactionBusy);
  const canUseMemberActions = Boolean(
    persistence === 'backend'
    && !interactionBusy
    && snapshot.enabled
    && team
    && !team.paused
    && !team.archived
    && team.revision !== undefined
    && !dirty
    && member?.nativeSessionId.trim(),
  );

  useEffect(() => {
    onDirtyChange?.(hasDirtyTeams);
    return () => onDirtyChange?.(false);
  }, [hasDirtyTeams, onDirtyChange]);

  useEffect(() => {
    setBootstrapPlan(null);
    setMemberActionMessage('');
  }, [selectedTeamId, selectedMemberId]);

  const markTeamDirty = (teamId: string) => {
    setDirtyTeamIds(current => {
      const next = new Set(current);
      next.add(teamId);
      return next;
    });
  };

  const markTeamClean = (teamId: string) => {
    setDirtyTeamIds(current => {
      if (!current.has(teamId)) return current;
      const next = new Set(current);
      next.delete(teamId);
      return next;
    });
  };

  const commitTeam = (next: CollaborationTeamDto) => {
    if (interactionBusy) return;
    setSnapshot(current => replaceTeam(current, next));
    markTeamDirty(next.id);
    setSavedPulse(false);
    setErrorKey(null);
  };

  const patchMember = (memberId: string, patch: Partial<CollaborationMemberDto>) => {
    if (!team || !team.paused || team.archived || saving || memberActionBusy) return;
    if (team.leader.id === memberId) {
      commitTeam({ ...team, leader: { ...team.leader, ...patch, role: 'leader' } });
      return;
    }
    commitTeam({
      ...team,
      workers: team.workers.map(item => item.id === memberId
        ? { ...item, ...patch, role: 'worker' }
        : item),
    });
  };

  const createAndBindGrokSession = () => {
    if (
      !team
      || !member
      || !canEdit
      || pendingNewSession
      || member.nativeSessionId.trim()
      || !team.workspace.trim()
    ) return;
    const terminalId = crypto.randomUUID();
    const nativeSessionId = crypto.randomUUID();
    setPendingNewSession({ terminalId, teamId: team.id, memberId: member.id, nativeSessionId });
    setMemberActionMessage(t('collab.member.session_new_waiting'));
    dispatch({
      type: 'ADD_TERMINAL',
      session: {
        id: terminalId,
        tool: 'grok',
        folderPath: team.workspace.trim(),
        newSessionToken: nativeSessionId,
        toolTitle: `${member.displayName} · ${t('nav.new_agent')}`,
        viewMode: 'terminal',
      },
    });
  };

  useEffect(() => {
    if (!pendingNewSession) return;
    const terminal = state.terminals.find(item => item.id === pendingNewSession.terminalId);
    const nativeSessionId = terminal?.resumeToken?.trim();
    if (nativeSessionId !== pendingNewSession.nativeSessionId) return;

    const target = snapshot.teams.find(item => item.id === pendingNewSession.teamId);
    const canBind = Boolean(
      target
      && target.paused
      && !target.archived
      && collaborationMembers(target).some(candidate => candidate.id === pendingNewSession.memberId)
      && !sessionBoundToAnotherMember(snapshot, nativeSessionId, pendingNewSession.memberId)
    );

    if (canBind && target) {
      const patch = (candidate: CollaborationMemberDto): CollaborationMemberDto => (
        candidate.id === pendingNewSession.memberId
          ? { ...candidate, nativeSessionId, status: 'offline' }
          : candidate
      );
      setSnapshot(current => {
        const currentTarget = current.teams.find(item => item.id === pendingNewSession.teamId) ?? target;
        return replaceTeam(current, {
          ...currentTarget,
          leader: patch(currentTarget.leader),
          workers: currentTarget.workers.map(patch),
        });
      });
      setDirtyTeamIds(current => new Set(current).add(pendingNewSession.teamId));
      setSessions(current => current.some(option => option.nativeSessionId === nativeSessionId)
        ? current
        : [...current, {
          nativeSessionId,
          label: terminal?.toolTitle || nativeSessionId,
          workspace: terminal?.folderPath || '',
          state: 'live',
        }]);
      setMemberActionMessage(t('collab.member.session_new_bound'));
    } else {
      setMemberActionMessage(t('collab.member.action_failed'));
    }
    const terminalId = pendingNewSession.terminalId;
    setPendingNewSession(null);
    // This was only a native-session creation tab. Closing it prevents the
    // ordinary-live collision that would otherwise block collaboration launch.
    window.setTimeout(() => dispatch({ type: 'REMOVE_TERMINAL', id: terminalId }), 1_000);
  }, [pendingNewSession, snapshot, state.terminals]);

  useEffect(() => {
    if (!pendingNewSession) return;
    const pending = pendingNewSession;
    const timer = window.setTimeout(() => {
      setPendingNewSession(null);
      setMemberActionMessage(t('collab.member.action_failed'));
      dispatch({ type: 'REMOVE_TERMINAL', id: pending.terminalId });
    }, 15_000);
    return () => window.clearTimeout(timer);
  }, [pendingNewSession]);

  const createTeam = () => {
    if (saving || memberActionBusy) return;
    const next = createCollaborationTeam(snapshot.teams.length + 1);
    setSnapshot(current => ({ ...current, teams: [...current.teams, next] }));
    setSelectedTeamId(next.id);
    setSelectedMemberId(next.leader.id);
    markTeamDirty(next.id);
    setSavedPulse(false);
    setErrorKey(null);
  };

  const addWorker = () => {
    if (!team || !team.paused || team.archived || saving || memberActionBusy || team.workers.length >= COLLABORATION_MAX_WORKERS) return;
    const worker = createCollaborationMember('worker', team.workers.length);
    commitTeam({ ...team, workers: [...team.workers, worker] });
    setSelectedMemberId(worker.id);
  };

  const removeWorker = (memberId: string) => {
    if (!team || !team.paused || team.archived || saving || memberActionBusy || team.leader.id === memberId || team.workers.length <= 1) return;
    const workers = team.workers.filter(item => item.id !== memberId);
    commitTeam({ ...team, workers });
    setSelectedMemberId(team.leader.id);
  };

  const promoteToLeader = (worker: CollaborationMemberDto) => {
    if (!team || !team.paused || team.archived || saving || memberActionBusy || worker.role === 'leader') return;
    const formerLeader: CollaborationMemberDto = { ...team.leader, role: 'worker' };
    const remainingWorkers = team.workers.filter(item => item.id !== worker.id);
    commitTeam({
      ...team,
      leader: { ...worker, role: 'leader' },
      workers: [formerLeader, ...remainingWorkers],
    });
    setSelectedMemberId(worker.id);
  };

  const applyBackendSnapshot = (
    nextSnapshot: CollaborationSnapshotDto,
    preferredTeamId: string | null = selectedTeamId,
    preferredMemberId: string | null = selectedMemberId,
  ) => {
    const nextTeam = nextSnapshot.teams.find(item => item.id === preferredTeamId)
      ?? nextSnapshot.teams.find(item => !item.archived)
      ?? nextSnapshot.teams[0]
      ?? null;
    const nextMembers = nextTeam ? collaborationMembers(nextTeam) : [];
    const nextMember = nextMembers.find(item => item.id === preferredMemberId)
      ?? nextTeam?.leader
      ?? null;
    setSnapshot(nextSnapshot);
    setSelectedTeamId(nextTeam?.id ?? null);
    setSelectedMemberId(nextMember?.id ?? null);
  };

  const reloadBackendSnapshot = async (
    preferredTeamId: string | null = selectedTeamId,
    preferredMemberId: string | null = selectedMemberId,
  ) => {
    const result = await loadCollaborationSettings();
    if (result.persistence !== 'backend') throw new Error(result.warning || 'backend unavailable');
    setPersistence('backend');
    applyBackendSnapshot(result.snapshot, preferredTeamId, preferredMemberId);
    return result.snapshot;
  };

  const saveTeam = async () => {
    if (!team || saving) return;
    const validation = validateTeam(team);
    if (validation) {
      setErrorKey(validation);
      return;
    }
    setSaving(true);
    setErrorKey(null);
    try {
      const normalized: CollaborationTeamDto = {
        ...team,
        name: team.name.trim(),
        workspace: team.workspace.trim(),
        leader: {
          ...team.leader,
          alias: team.leader.alias.trim(),
          displayName: team.leader.displayName.trim(),
          nativeSessionId: team.leader.nativeSessionId.trim(),
        },
        workers: team.workers.map(item => ({
          ...item,
          alias: item.alias.trim(),
          displayName: item.displayName.trim(),
          nativeSessionId: item.nativeSessionId.trim(),
        })),
      };
      const saved = await saveCollaborationTeam(normalized, snapshot, persistence);
      setSnapshot(current => ({
        ...current,
        teams: current.teams.some(item => item.id === team.id)
          ? current.teams.map(item => item.id === team.id ? saved : item)
          : [...current.teams, saved],
      }));
      setSelectedTeamId(saved.id);
      const savedMember = member?.role === 'worker'
        ? saved.workers.find(item => item.alias === member.alias) ?? saved.leader
        : saved.leader;
      setSelectedMemberId(savedMember.id);
      setDirtyTeamIds(current => {
        const next = new Set(current);
        next.delete(team.id);
        next.delete(saved.id);
        return next;
      });
      setSavedPulse(true);
      window.setTimeout(() => setSavedPulse(false), 1800);
    } catch {
      setErrorKey('collab.error.save');
    } finally {
      setSaving(false);
    }
  };

  const toggleGlobal = async () => {
    if (persistence !== 'backend' || interactionBusy || (!snapshot.enabled && hasDirtyTeams)) return;
    const enabled = !snapshot.enabled;
    setSaving(true);
    setErrorKey(null);
    try {
      await setCollaborationEnabled(enabled, persistence);
      if (!enabled && hasDirtyTeams) {
        // Stopping collaboration is a safety action and must remain available,
        // but a backend refresh here would overwrite unrelated local drafts.
        setSnapshot(current => ({ ...current, enabled: false }));
      } else {
        await reloadBackendSnapshot(selectedTeamId, selectedMemberId);
      }
    } catch {
      setErrorKey('collab.error.save');
    } finally {
      setSaving(false);
    }
  };

  const togglePause = async () => {
    if (!team || team.archived || interactionBusy || (team.paused && dirty)) return;
    if (team.paused && (persistence !== 'backend' || !snapshot.enabled)) {
      setErrorKey('collab.error.mode_off');
      return;
    }
    if (team.paused && collaborationMembers(team).some(item => !item.nativeSessionId.trim())) {
      setErrorKey('collab.error.bind_all');
      return;
    }
    setSaving(true);
    setErrorKey(null);
    try {
      const updated = await setCollaborationTeamPaused(team, !team.paused, snapshot, persistence);
      if (persistence === 'backend') {
        if (hasDirtyTeams) {
          setSnapshot(current => replaceTeam(current, updated));
        } else {
          await reloadBackendSnapshot(team.id, selectedMemberId);
        }
      } else {
        setSnapshot(current => replaceTeam(current, updated));
      }
      markTeamClean(team.id);
    } catch {
      setErrorKey('collab.error.save');
    } finally {
      setSaving(false);
    }
  };

  const archiveTeam = async () => {
    if (!team || team.archived || interactionBusy || dirty) return;
    if (!window.confirm(t('collab.archive.confirm'))) return;
    setSaving(true);
    setErrorKey(null);
    try {
      await archiveCollaborationTeam(team, snapshot, persistence);
      const archived = { ...team, archived: true, paused: true };
      if (persistence === 'backend') {
        if (hasDirtyTeams) {
          setSnapshot(current => replaceTeam(current, archived));
        } else {
          await reloadBackendSnapshot(team.id, selectedMemberId);
        }
      } else {
        setSnapshot(current => replaceTeam(current, archived));
      }
      markTeamClean(team.id);
    } catch {
      setErrorKey('collab.error.save');
    } finally {
      setSaving(false);
    }
  };

  const refreshOffice = async () => {
    if (persistence !== 'backend' || refreshingOffice || hasDirtyTeams) return;
    setRefreshingOffice(true);
    setErrorKey(null);
    try {
      await reloadBackendSnapshot(selectedTeamId, selectedMemberId);
    } catch {
      setErrorKey('collab.error.load');
    } finally {
      setRefreshingOffice(false);
    }
  };

  useEffect(() => {
    if (view !== 'office' || persistence !== 'backend' || hasDirtyTeams || saving) return;
    const timer = window.setInterval(() => { void refreshOffice(); }, 3000);
    return () => window.clearInterval(timer);
  }, [view, persistence, hasDirtyTeams, saving, selectedTeamId]);

  const launchMember = async () => {
    if (!team || !member || !canUseMemberActions || memberActionBusy) return;
    if (team.revision === undefined) return;
    setMemberActionBusy(true);
    setMemberActionMessage('');
    setBootstrapPlan(null);
    try {
      const plan = await getCollaborationMemberLaunchPlan(
        team.id,
        member.id,
        team.revision,
        persistence,
      );
      if (plan.status === 'blocked') {
        setMemberActionMessage(t('collab.member.action_failed'));
        return;
      }
      if (plan.status === 'ordinary_live_collision') {
        if (plan.terminalSessionId) {
          dispatch({ type: 'SET_ACTIVE_TERMINAL', id: plan.terminalSessionId });
        }
        setMemberActionMessage(t('collab.member.collision'));
        return;
      }
      if (plan.status === 'already_collaboration_active') {
        const live = plan.terminalSessionId
          ? state.terminals.find(terminal => terminal.id === plan.terminalSessionId)
          : undefined;
        if (
          live?.tool === 'grok'
          && live.resumeToken === plan.nativeSessionId
        ) {
          dispatch({
            type: 'SET_RESUME_TOKEN',
            id: live.id,
            token: plan.nativeSessionId,
            exactResumeToken: true,
          });
          dispatch({ type: 'SET_ACTIVE_TERMINAL', id: live.id });
          dispatch({ type: 'SET_SETTINGS_OPEN', open: false });
        } else {
          setMemberActionMessage(t('collab.member.action_failed'));
        }
        return;
      }

      // Close the small race where a tab has been created in React but its
      // native PTY has not reached the Rust registry queried by the plan.
      const pendingCollision = state.terminals.find(terminal => (
        !terminal.isHidden
        && terminal.tool === 'grok'
        && terminal.resumeToken === plan.nativeSessionId
      ));
      if (pendingCollision) {
        dispatch({ type: 'SET_ACTIVE_TERMINAL', id: pendingCollision.id });
        setMemberActionMessage(t('collab.member.collision'));
        return;
      }

      const idle = state.terminals.find(terminal => !terminal.tool && !terminal.isHidden);
      if (idle) {
        dispatch({ type: 'SET_ACTIVE_TERMINAL', id: idle.id });
        dispatch({ type: 'SET_FOLDER', path: plan.workspace });
        dispatch({
          type: 'SET_TERMINAL_TOOL',
          id: idle.id,
          tool: 'grok',
          resumeToken: plan.nativeSessionId,
          exactResumeToken: true,
        });
        dispatch({ type: 'SET_TAB_TITLE', id: idle.id, title: plan.terminalTitle });
      } else {
        dispatch({
          type: 'ADD_TERMINAL',
          session: {
            id: crypto.randomUUID(),
            tool: 'grok',
            folderPath: plan.workspace,
            resumeToken: plan.nativeSessionId,
            exactResumeToken: true,
            toolTitle: plan.terminalTitle,
            viewMode: 'terminal',
          },
        });
      }
      // The Grok TUI remains the visible authority for launch errors and
      // trust prompts. Do not keep the settings sheet over it.
      dispatch({ type: 'SET_SETTINGS_OPEN', open: false });
    } catch {
      setMemberActionMessage(t('collab.member.action_failed'));
    } finally {
      setMemberActionBusy(false);
    }
  };

  const initializeMember = async () => {
    if (!team || !member || !canUseMemberActions || memberActionBusy) return;
    if (team.revision === undefined) return;
    setMemberActionBusy(true);
    setMemberActionMessage('');
    setBootstrapPlan(null);
    try {
      const launch = await getCollaborationMemberLaunchPlan(
        team.id,
        member.id,
        team.revision,
        persistence,
      );
      if (launch.status === 'ordinary_live_collision') {
        if (launch.terminalSessionId) {
          dispatch({ type: 'SET_ACTIVE_TERMINAL', id: launch.terminalSessionId });
        }
        setMemberActionMessage(t('collab.member.collision'));
        return;
      }
      if (
        launch.status !== 'already_collaboration_active'
        || !launch.terminalSessionId
        || launch.runtimeGeneration === undefined
      ) {
        setMemberActionMessage(t('collab.member.launch_first'));
        return;
      }
      const target = state.terminals.find(terminal => terminal.id === launch.terminalSessionId);
      const actions = getTabActions(launch.terminalSessionId);
      const safety = actions?.bootstrapSafety();
      if (
        !target
        || target.tool !== 'grok'
        || target.resumeToken !== launch.nativeSessionId
      ) {
        dispatch({ type: 'SET_ACTIVE_TERMINAL', id: launch.terminalSessionId });
        setMemberActionMessage(t('collab.member.action_failed'));
        return;
      }
      if (!target.exactResumeToken) {
        dispatch({
          type: 'SET_RESUME_TOKEN',
          id: target.id,
          token: launch.nativeSessionId,
          exactResumeToken: true,
        });
      }
      const plan = await beginCollaborationBootstrap(
        team.id,
        member.id,
        launch.terminalSessionId,
        launch.runtimeGeneration,
        persistence,
      );
      if (plan.status === 'already_ready') {
        setMemberActionMessage(t('collab.member.bootstrap_ready'));
        return;
      }
      if (!plan.prompt?.trim()) {
        setMemberActionMessage(t('collab.member.action_failed'));
        return;
      }
      dispatch({ type: 'SET_ACTIVE_TERMINAL', id: plan.terminalSessionId });
      setBootstrapPlan(plan);
      if (
        !actions
        || target.agentStatus === 'working'
        || target.agentStatus === 'wait_input'
        || Boolean(target.chatPending)
        || Boolean(target.gambitDraft?.trim())
        || !safety?.ready
        || safety.clean !== true
      ) {
        // Still show the backend-generated exact prompt so the user can copy
        // it and submit manually after resolving a trust/permission/draft
        // state. Automatic submission remains fail-closed.
        setMemberActionMessage(t('collab.member.bootstrap_unsafe'));
      }
    } catch {
      setMemberActionMessage(t('collab.member.action_failed'));
    } finally {
      setMemberActionBusy(false);
    }
  };

  const submitBootstrap = async () => {
    if (!bootstrapPlan || !team || !member || memberActionBusy) return;
    const target = state.terminals.find(terminal => terminal.id === bootstrapPlan.terminalSessionId);
    const actions = getTabActions(bootstrapPlan.terminalSessionId);
    const safety = actions?.bootstrapSafety();
    if (
      !target
      || !actions
      || target.agentStatus === 'working'
      || target.agentStatus === 'wait_input'
      || Boolean(target.chatPending)
      || Boolean(target.gambitDraft?.trim())
      || !safety?.ready
      || safety.clean !== true
      || !bootstrapPlan.prompt
    ) {
      setBootstrapPlan(null);
      setMemberActionMessage(t('collab.member.bootstrap_unsafe'));
      return;
    }
    setMemberActionBusy(true);
    setMemberActionMessage('');
    let submitted = false;
    try {
      // Preview and submit are separated by user think time. Re-authorize the
      // exact team/member/terminal/generation immediately before touching the
      // PTY so a pause, rebind, restart, or listener race fails closed.
      const authorized = await beginCollaborationBootstrap(
        team.id,
        member.id,
        bootstrapPlan.terminalSessionId,
        bootstrapPlan.runtimeGeneration,
        persistence,
      );
      if (authorized.status === 'already_ready') {
        setBootstrapPlan(null);
        setMemberActionMessage(t('collab.member.bootstrap_ready'));
        return;
      }
      if (!sameBootstrapAuthorization(bootstrapPlan, authorized)) {
        setBootstrapPlan(null);
        setMemberActionMessage(t('collab.member.action_failed'));
        return;
      }
      const latestActions = getTabActions(bootstrapPlan.terminalSessionId);
      const latestSafety = latestActions?.bootstrapSafety();
      if (!latestActions || !latestSafety?.ready || latestSafety.clean !== true) {
        setBootstrapPlan(null);
        setMemberActionMessage(t('collab.member.bootstrap_unsafe'));
        return;
      }
      dispatch({ type: 'SET_ACTIVE_TERMINAL', id: bootstrapPlan.terminalSessionId });
      submitted = await latestActions.submitVisiblePrompt(bootstrapPlan.prompt);
    } catch {
      setBootstrapPlan(null);
      setMemberActionMessage(t('collab.member.action_failed'));
      return;
    } finally {
      setMemberActionBusy(false);
    }
    if (!submitted) {
      setBootstrapPlan(null);
      setMemberActionMessage(t('collab.member.bootstrap_unsafe'));
      return;
    }
    setBootstrapPlan(null);
    dispatch({ type: 'SET_SETTINGS_OPEN', open: false });
  };

  const copyBootstrapForManualSubmit = async () => {
    if (!bootstrapPlan?.prompt || !team || !member || memberActionBusy) return;
    const preview = bootstrapPlan;
    setMemberActionBusy(true);
    setMemberActionMessage('');
    try {
      const authorized = await beginCollaborationBootstrap(
        team.id,
        member.id,
        preview.terminalSessionId,
        preview.runtimeGeneration,
        persistence,
      );
      if (authorized.status === 'already_ready') {
        setBootstrapPlan(null);
        setMemberActionMessage(t('collab.member.bootstrap_ready'));
        return;
      }
      if (!sameBootstrapAuthorization(preview, authorized) || !authorized.prompt) {
        setBootstrapPlan(null);
        setMemberActionMessage(t('collab.member.action_failed'));
        return;
      }
      await clipboardWrite(authorized.prompt);
      dispatch({ type: 'SET_ACTIVE_TERMINAL', id: authorized.terminalSessionId });
      setBootstrapPlan(null);
      dispatch({ type: 'SET_SETTINGS_OPEN', open: false });
    } catch {
      setMemberActionMessage(t('collab.member.action_failed'));
    } finally {
      setMemberActionBusy(false);
    }
  };

  if (loading) {
    return <div className="collab-loading" role="status">{t('collab.loading')}</div>;
  }

  return (
    <div className="collab-settings">
      <div className="collab-mode-row">
        <div className="collab-mode-copy">
          <div className="collab-mode-title-row">
            <span className="collab-mode-title">{t('collab.mode.title')}</span>
            <span className={`collab-mode-state${snapshot.enabled ? ' is-on' : ''}`}>
              {snapshot.enabled ? t('collab.mode.on') : t('collab.mode.off')}
            </span>
          </div>
          <p>{t('collab.mode.description')}</p>
        </div>
        <button
          type="button"
          className="collab-switch"
          role="switch"
          aria-checked={persistence === 'backend' && snapshot.enabled}
          aria-label={t('collab.mode.title')}
          disabled={persistence !== 'backend' || interactionBusy || (!snapshot.enabled && hasDirtyTeams)}
          onClick={toggleGlobal}
        >
          <span />
        </button>
      </div>

      {persistence === 'draft' && (
        <div className="collab-draft-notice" role="status">
          <span className="collab-draft-pixel" aria-hidden="true" />
          <span><strong>{t('collab.draft.title')}</strong>{t('collab.draft.description')}</span>
        </div>
      )}

      <div className="collab-view-switch" aria-label={t('settings.collaboration')}>
        <button
          type="button"
          className={view === 'editor' ? 'is-active' : ''}
          aria-pressed={view === 'editor'}
          disabled={interactionBusy}
          onClick={() => setView('editor')}
        >
          <span className="collab-view-editor-icon" aria-hidden="true"><i /><i /><i /></span>
          {t('collab.view.editor')}
        </button>
        <button
          type="button"
          className={view === 'office' ? 'is-active' : ''}
          aria-pressed={view === 'office'}
          disabled={interactionBusy}
          onClick={() => setView('office')}
        >
          <span className="collab-view-office-icon" aria-hidden="true"><i /><i /><i /></span>
          {t('collab.view.office')}
        </button>
      </div>

      <div className="collab-team-strip" aria-label={t('collab.teams')}>
        <div className="collab-team-tabs">
          {snapshot.teams.map(item => (
            <button
              key={item.id}
              type="button"
              className={`collab-team-tab${item.id === selectedTeamId ? ' is-active' : ''}${item.archived ? ' is-archived' : ''}`}
              disabled={interactionBusy}
              onClick={() => {
                setSelectedTeamId(item.id);
                setSelectedMemberId(item.leader.id);
                setErrorKey(null);
              }}
            >
              <span className={`collab-team-dot${item.archived ? ' is-archived' : item.paused ? ' is-paused' : ' is-ready'}`} />
              <span>{item.name}</span>
            </button>
          ))}
        </div>
        {view === 'editor' && (
          <button type="button" className="collab-new-team" onClick={createTeam} disabled={interactionBusy}>
            <span aria-hidden="true">＋</span>{t('collab.team.new')}
          </button>
        )}
      </div>

      {view === 'office' ? (
        <CollaborationOffice
          snapshot={snapshot}
          team={team}
          persistence={persistence}
          refreshing={refreshingOffice}
          editingPreview={hasDirtyTeams}
          refreshDisabled={hasDirtyTeams || saving}
          editDisabled={interactionBusy}
          onRefresh={() => { void refreshOffice(); }}
          onEdit={() => setView('editor')}
        />
      ) : !team ? (
        <div className="collab-empty">
          <div className="collab-empty-roster" aria-hidden="true">
            <PixelAvatar avatarId="cedar" animated />
            <PixelAvatar avatarId="moss" animated />
            <PixelAvatar avatarId="plum" animated />
          </div>
          <strong>{t('collab.empty.title')}</strong>
          <p>{t('collab.empty.description')}</p>
          <button type="button" className="collab-primary-btn" onClick={createTeam} disabled={interactionBusy}>{t('collab.team.new')}</button>
        </div>
      ) : (
        <>
          <div className="collab-team-heading">
            <label className="collab-field collab-team-name-field">
              <span>{t('collab.team.name')}</span>
              <input
                value={team.name}
                disabled={!canEdit}
                maxLength={64}
                onChange={event => commitTeam({ ...team, name: event.target.value })}
              />
            </label>
            <span className="collab-provider-mark">Grok Build</span>
          </div>

          <label className="collab-field">
            <span>{t('collab.team.workspace')}</span>
            <input
              value={team.workspace}
              disabled={!canEdit}
              placeholder={t('collab.team.workspace_placeholder')}
              onChange={event => commitTeam({ ...team, workspace: event.target.value })}
            />
          </label>

          {!team.paused && (
            <div className="collab-edit-notice" role="status">{t('collab.edit.pause_first')}</div>
          )}

          <div className="collab-roster-heading">
            <div>
              <span className="collab-eyebrow">{t('collab.roster')}</span>
              <span className="collab-roster-count">1 + {team.workers.length}/{COLLABORATION_MAX_WORKERS}</span>
            </div>
            <div className="collab-team-actions">
              {!team.archived && (
                <button type="button" className="collab-quiet-btn" onClick={togglePause} disabled={interactionBusy || (team.paused && dirty)}>
                  {team.paused ? t('collab.team.enable') : t('collab.team.pause')}
                </button>
              )}
              <button type="button" className="collab-quiet-btn is-danger" onClick={archiveTeam} disabled={interactionBusy || dirty || team.archived}>
                {team.archived ? t('collab.team.archived') : t('collab.team.archive')}
              </button>
            </div>
          </div>

          <div className="collab-roster" aria-label={t('collab.roster')}>
            {members.map(item => (
              <button
                key={item.id}
                type="button"
                className={`collab-station${item.id === member?.id ? ' is-selected' : ''}`}
                disabled={interactionBusy}
                onClick={() => setSelectedMemberId(item.id)}
              >
                <span className="collab-station-scene">
                  {item.role === 'leader' && <span className="collab-leader-flag" aria-label={t('collab.member.leader')}>L</span>}
                  <PixelAvatar avatarId={item.avatarId} pose="seat" animated />
                  <span className="collab-desk"><span /></span>
                </span>
                <span className="collab-station-name">{item.displayName || item.alias}</span>
                <span className="collab-station-meta">
                  <span className={`collab-status-dot is-${item.status}`} />
                  {t(STATUS_KEY[item.status])}
                </span>
              </button>
            ))}
            {canEdit && team.workers.length < COLLABORATION_MAX_WORKERS && (
              <button type="button" className="collab-station collab-add-station" onClick={addWorker} disabled={saving}>
                <span className="collab-add-plus" aria-hidden="true">＋</span>
                <span>{t('collab.member.add')}</span>
              </button>
            )}
          </div>

          {member && (
            <div className="collab-member-editor" aria-label={t('collab.member.details')}>
              <div className="collab-editor-title">
                <div>
                  <span className="collab-eyebrow">{t('collab.member.details')}</span>
                  <strong>{member.displayName || member.alias}</strong>
                </div>
                {member.role === 'worker' && canEdit && team.workers.length > 1 && (
                  <button type="button" className="collab-text-btn" onClick={() => removeWorker(member.id)} disabled={saving}>
                    {t('collab.member.remove')}
                  </button>
                )}
              </div>

              <div className="collab-form-grid">
                <label className="collab-field">
                  <span>{t('collab.member.name')}</span>
                  <input
                    value={member.displayName}
                    disabled={!canEdit}
                    maxLength={48}
                    onChange={event => patchMember(member.id, { displayName: event.target.value })}
                  />
                </label>
                <label className="collab-field">
                  <span>{t('collab.member.alias')}</span>
                  <input
                    value={member.alias}
                    disabled={!canEdit}
                    maxLength={32}
                    spellCheck={false}
                    onChange={event => patchMember(member.id, { alias: event.target.value.toLowerCase().replace(/\s+/g, '-') })}
                  />
                </label>
              </div>

              <div className="collab-role-row" aria-label={t('collab.member.role')}>
                <span>{t('collab.member.role')}</span>
                <button type="button" className={member.role === 'leader' ? 'is-active' : ''} disabled={!canEdit} onClick={() => member.role === 'worker' && promoteToLeader(member)}>
                  {t('collab.member.leader')}
                </button>
                <button type="button" className={member.role === 'worker' ? 'is-active' : ''} disabled>
                  {t('collab.member.worker')}
                </button>
              </div>

              <div className="collab-field">
                <span>{t('collab.member.session')}</span>
                <div className="collab-session-picker">
                  <select
                    value={member.nativeSessionId}
                    disabled={!canEdit || Boolean(pendingNewSession)}
                    onChange={event => patchMember(member.id, {
                      nativeSessionId: event.target.value,
                      status: event.target.value ? 'offline' : 'unbound',
                    })}
                  >
                    <option value="">{t('collab.member.session_select')}</option>
                    {member.nativeSessionId && !sessions.some(option => option.nativeSessionId === member.nativeSessionId) && (
                      <option value={member.nativeSessionId}>{member.nativeSessionId}</option>
                    )}
                    {sessions.map(option => {
                      const taken = sessionBoundToAnotherMember(snapshot, option.nativeSessionId, member.id);
                      const detail = option.workspace && option.workspace !== team.workspace
                        ? `${option.label} · ${option.workspace}`
                        : option.label;
                      return (
                        <option key={option.nativeSessionId} value={option.nativeSessionId} disabled={taken}>
                          {detail}
                        </option>
                      );
                    })}
                  </select>
                  <button
                    type="button"
                    className="collab-session-new"
                    disabled={!canEdit || Boolean(pendingNewSession) || Boolean(member.nativeSessionId.trim()) || !team.workspace.trim()}
                    onClick={createAndBindGrokSession}
                  >
                    {t('nav.new_agent')}
                  </button>
                </div>
                <small>{sessions.length ? t('collab.member.session_hint') : t('collab.member.no_sessions')}</small>
                {pendingNewSession?.memberId === member.id && (
                  <small className="collab-session-progress" role="status">{t('collab.member.session_new_waiting')}</small>
                )}
                <details className="collab-session-manual">
                  <summary>{t('collab.member.session_manual')}</summary>
                  <input
                    value={member.nativeSessionId}
                    disabled={!canEdit || Boolean(pendingNewSession)}
                    placeholder={t('collab.member.session_placeholder')}
                    spellCheck={false}
                    onChange={event => patchMember(member.id, {
                      nativeSessionId: event.target.value,
                      status: event.target.value ? 'offline' : 'unbound',
                    })}
                  />
                </details>
              </div>

              <div className="collab-member-actions" aria-label={t('collab.member.connection_actions')}>
                <button type="button" disabled={!canUseMemberActions || memberActionBusy || saving} onClick={() => { void launchMember(); }}>
                  {t('collab.member.launch')}
                </button>
                <button type="button" disabled={!canUseMemberActions || memberActionBusy || saving} onClick={() => { void initializeMember(); }}>
                  {t('collab.member.initialize')}
                </button>
                <small>{t('collab.member.actions_available')}</small>
                {memberActionMessage && <span className="collab-member-action-message" role="status">{memberActionMessage}</span>}
              </div>

              {bootstrapPlan?.prompt && (
                <div className="collab-bootstrap-confirm" role="dialog" aria-label={t('collab.member.initialize')}>
                  <strong>{t('collab.member.bootstrap_confirm')}</strong>
                  <p>{t('collab.member.bootstrap_cost')}</p>
                  <pre>{bootstrapPlan.prompt}</pre>
                  <div>
                    <button type="button" className="collab-quiet-btn" onClick={() => setBootstrapPlan(null)} disabled={memberActionBusy || saving}>
                      {t('collab.member.bootstrap_cancel')}
                    </button>
                    <button type="button" className="collab-quiet-btn" onClick={() => { void copyBootstrapForManualSubmit(); }} disabled={memberActionBusy || saving}>
                      {t('menu.copy')}
                    </button>
                    <button type="button" className="collab-primary-btn" onClick={() => { void submitBootstrap(); }} disabled={memberActionBusy || saving}>
                      {t('collab.member.bootstrap_submit')}
                    </button>
                  </div>
                </div>
              )}

              <fieldset className="collab-avatar-field" disabled={!canEdit}>
                <legend>{t('collab.member.avatar')}</legend>
                <div className="collab-avatar-grid">
                  {PIXEL_AVATARS.map(profile => (
                    <button
                      key={profile.id}
                      type="button"
                      className={member.avatarId === profile.id ? 'is-active' : ''}
                      aria-pressed={member.avatarId === profile.id}
                      onClick={() => patchMember(member.id, { avatarId: profile.id })}
                    >
                      <PixelAvatar avatarId={profile.id} animated={member.avatarId === profile.id} />
                      <span>{profile.name}</span>
                    </button>
                  ))}
                </div>
              </fieldset>
            </div>
          )}

          <div className="collab-save-row">
            <span className="collab-save-message" role="status">
              {errorKey ? t(errorKey) : savedPulse ? t('collab.saved') : dirty ? t('collab.unsaved') : ''}
            </span>
            <button type="button" className="collab-primary-btn" onClick={saveTeam} disabled={saving || !canEdit || !dirty}>
              {saving ? t('collab.saving') : t('tool_config.save')}
            </button>
          </div>
        </>
      )}
    </div>
  );
}
