import { useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import type { I18nKey } from '../../i18n/en';
import { useT } from '../../i18n/useT';
import { PixelAvatar, type PixelAvatarFacing } from './PixelAvatar';
import {
  collaborationMembers,
  type CollaborationMemberDto,
  type CollaborationMemberStatus,
  type CollaborationPersistence,
  type CollaborationReportDeliveryDto,
  type CollaborationReportOutcome,
  type CollaborationSnapshotDto,
  type CollaborationTeamDto,
} from './types';
import './CollaborationOffice.css';

const STATUS_KEY: Record<CollaborationMemberStatus, I18nKey> = {
  unbound: 'collab.status.unbound',
  connecting: 'collab.status.connecting',
  ready: 'collab.status.ready',
  busy: 'collab.status.busy',
  waiting_user: 'collab.status.waiting_user',
  offline: 'collab.status.offline',
  error: 'collab.status.error',
};

const LEADER_LEFT = 34;
const LEADER_TOP = 42;
const WORKER_TOP = 75;
const HALL_TOP = 52;
const GO_MS = 2200;
const HANDOFF_MS = 700;
const BACK_MS = 2000;
const FRESH_REPORT_MS = 120_000;

interface CollaborationOfficeProps {
  snapshot: CollaborationSnapshotDto;
  team: CollaborationTeamDto | null;
  persistence: CollaborationPersistence;
  refreshing: boolean;
  editingPreview: boolean;
  refreshDisabled: boolean;
  editDisabled: boolean;
  onRefresh: () => void;
  onEdit: () => void;
}

function officeState(
  snapshot: CollaborationSnapshotDto,
  team: CollaborationTeamDto,
): { key: I18nKey; className: string } {
  if (team.archived) return { key: 'collab.office.state.archived', className: 'is-archived' };
  if (!snapshot.enabled) return { key: 'collab.office.state.off', className: 'is-off' };
  if (team.paused) return { key: 'collab.office.state.paused', className: 'is-paused' };
  return { key: 'collab.office.state.live', className: 'is-live' };
}

function workerLeftPercent(index: number, count: number): number {
  const positions = count === 1 ? [28] : count === 2 ? [18, 42] : [12, 28, 44];
  return positions[index] ?? 28;
}

function prefersReducedMotion(): boolean {
  return typeof window !== 'undefined'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function facingFor(dx: number, dy: number): PixelAvatarFacing {
  if (Math.abs(dx) > Math.abs(dy)) return dx < 0 ? 'west' : 'east';
  return dy < 0 ? 'north' : 'south';
}

function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

function followPath(
  points: Array<{ x: number; y: number }>,
  t: number,
): { x: number; y: number; facing: PixelAvatarFacing } {
  const last = points[points.length - 1] ?? { x: LEADER_LEFT, y: LEADER_TOP };
  if (points.length < 2) return { x: last.x, y: last.y, facing: 'north' };
  const clamped = Math.min(1, Math.max(0, t));
  const scaled = clamped * (points.length - 1);
  const index = Math.min(points.length - 2, Math.floor(scaled));
  const local = scaled - index;
  const from = points[index];
  const to = points[index + 1];
  return {
    x: lerp(from.x, to.x, local),
    y: lerp(from.y, to.y, local),
    facing: facingFor(to.x - from.x, to.y - from.y),
  };
}

function outboundPath(fromLeft: number): Array<{ x: number; y: number }> {
  return [
    { x: fromLeft, y: WORKER_TOP },
    { x: fromLeft, y: HALL_TOP },
    { x: LEADER_LEFT, y: HALL_TOP },
    { x: LEADER_LEFT, y: LEADER_TOP },
  ];
}

interface CourierPose {
  runId: string;
  workerId: string;
  outcome: CollaborationReportOutcome;
  left: number;
  top: number;
  facing: PixelAvatarFacing;
  carrying: boolean;
  handingOff: boolean;
  preview: boolean;
}

function OfficeSeat({
  member,
  active,
  statusLabel,
  away,
  receiving,
  workerIndex,
  workerCount,
}: {
  member: CollaborationMemberDto;
  active: boolean;
  statusLabel: string;
  away: boolean;
  receiving: boolean;
  workerIndex?: number;
  workerCount?: number;
}) {
  const isLeader = member.role === 'leader';
  const animated = !away && active && (member.status === 'ready'
    || member.status === 'busy'
    || member.status === 'connecting');
  const style = workerIndex === undefined
    ? undefined
    : ({ '--office-worker-left': `${workerLeftPercent(workerIndex, workerCount ?? 1)}%` } as CSSProperties);
  const showChip = !away && (member.status === 'ready'
    || member.status === 'busy'
    || member.status === 'connecting'
    || member.status === 'waiting_user'
    || member.status === 'error');

  return (
    <div
      className={`collab-office-seat ${isLeader ? 'is-leader' : 'is-worker'} is-${member.status}${away ? ' is-away' : ''}${receiving ? ' is-receiving' : ''}`}
      style={style}
      aria-hidden="true"
    >
      {isLeader && <span className="collab-office-leader-pennant">L</span>}
      {showChip && (
        <span className={`collab-office-status-chip is-${member.status}`}>
          {statusLabel}
        </span>
      )}
      {away
        ? <span className="collab-office-empty-chair" />
        : <PixelAvatar avatarId={member.avatarId} pose="seat" animated={animated} />}
      <span className="collab-office-desk-top">
        <span className="collab-office-monitor" />
        <span className="collab-office-mug" />
        {receiving && <span className="collab-office-desk-report" />}
      </span>
      <span className="collab-office-seat-label">
        <strong>{member.displayName || member.alias}</strong>
      </span>
    </div>
  );
}

export function CollaborationOffice({
  snapshot,
  team,
  persistence,
  refreshing,
  editingPreview,
  refreshDisabled,
  editDisabled,
  onRefresh,
  onEdit,
}: CollaborationOfficeProps) {
  const t = useT();
  const [courier, setCourier] = useState<CourierPose | null>(null);
  const seenReportIds = useRef<Set<string>>(new Set());
  const primed = useRef(false);
  const queue = useRef<Array<CollaborationReportDeliveryDto & { preview?: boolean }>>([]);
  const running = useRef(false);
  const cancelRun = useRef<(() => void) | null>(null);

  const reports = team?.recentReports;
  const liveOffice = !!team && snapshot.enabled && !team.paused && !team.archived && persistence === 'backend';

  const playRun = (
    delivery: CollaborationReportDeliveryDto & { preview?: boolean },
    currentTeam: CollaborationTeamDto,
  ) => {
    const workerIndex = currentTeam.workers.findIndex(item => item.id === delivery.workerMemberId);
    if (workerIndex < 0) {
      running.current = false;
      return;
    }
    const fromLeft = workerLeftPercent(workerIndex, currentTeam.workers.length);
    const go = outboundPath(fromLeft);
    const back = [...go].reverse();
    const reduced = prefersReducedMotion();
    running.current = true;
    if (reduced) {
      setCourier({
        runId: delivery.id,
        workerId: delivery.workerMemberId,
        outcome: delivery.outcome,
        left: LEADER_LEFT,
        top: LEADER_TOP,
        facing: 'north',
        carrying: false,
        handingOff: true,
        preview: !!delivery.preview,
      });
      const timer = window.setTimeout(() => {
        setCourier(null);
        running.current = false;
        cancelRun.current = null;
      }, 700);
      cancelRun.current = () => window.clearTimeout(timer);
      return;
    }
    const started = performance.now();

    const tick = (now: number) => {
      const elapsed = now - started;
      if (reduced || elapsed >= GO_MS + HANDOFF_MS + BACK_MS) {
        setCourier(null);
        running.current = false;
        cancelRun.current = null;
        return;
      }
      if (elapsed < GO_MS) {
        const pose = followPath(go, elapsed / GO_MS);
        setCourier({
          runId: delivery.id,
          workerId: delivery.workerMemberId,
          outcome: delivery.outcome,
          left: pose.x,
          top: pose.y,
          facing: pose.facing,
          carrying: true,
          handingOff: false,
          preview: !!delivery.preview,
        });
      } else if (elapsed < GO_MS + HANDOFF_MS) {
        setCourier({
          runId: delivery.id,
          workerId: delivery.workerMemberId,
          outcome: delivery.outcome,
          left: LEADER_LEFT,
          top: LEADER_TOP,
          facing: 'north',
          carrying: elapsed < GO_MS + HANDOFF_MS / 2,
          handingOff: true,
          preview: !!delivery.preview,
        });
      } else {
        const pose = followPath(back, (elapsed - GO_MS - HANDOFF_MS) / BACK_MS);
        setCourier({
          runId: delivery.id,
          workerId: delivery.workerMemberId,
          outcome: delivery.outcome,
          left: pose.x,
          top: pose.y,
          facing: pose.facing,
          carrying: false,
          handingOff: false,
          preview: !!delivery.preview,
        });
      }
      const frame = requestAnimationFrame(tick);
      cancelRun.current = () => cancelAnimationFrame(frame);
    };
    const frame = requestAnimationFrame(tick);
    cancelRun.current = () => cancelAnimationFrame(frame);
  };

  const pumpQueue = (currentTeam: CollaborationTeamDto) => {
    if (running.current) return;
    const next = queue.current.shift();
    if (!next) return;
    playRun(next, currentTeam);
  };

  useEffect(() => {
    primed.current = false;
    seenReportIds.current = new Set();
    queue.current = [];
    cancelRun.current?.();
    running.current = false;
    setCourier(null);
  }, [team?.id]);

  useEffect(() => {
    if (!team) return;
    const known = reports ?? [];
    if (!primed.current) {
      const cutoff = Date.now() - FRESH_REPORT_MS;
      for (const report of known) {
        if (report.deliveredAt < cutoff) seenReportIds.current.add(report.id);
      }
      primed.current = true;
    }
    if (!liveOffice) {
      queue.current = queue.current.filter(item => item.preview);
      return;
    }
    const fresh = [...known].sort((a, b) => a.deliveredAt - b.deliveredAt);
    for (const report of fresh) {
      if (seenReportIds.current.has(report.id)) continue;
      seenReportIds.current.add(report.id);
      queue.current.push(report);
    }
    pumpQueue(team);
  }, [liveOffice, reports, team]);

  useEffect(() => {
    if (!team || running.current || queue.current.length === 0) return;
    pumpQueue(team);
  }, [courier, team]);

  useEffect(() => () => {
    cancelRun.current?.();
  }, []);

  useEffect(() => {
    if (!import.meta.env.DEV) return;
    const onPreview = () => {
      if (!team?.workers[0]) return;
      queue.current.push({
        id: `preview-${Date.now()}`,
        workerMemberId: team.workers[0].id,
        outcome: 'completed',
        deliveredAt: Date.now(),
        preview: true,
      });
      pumpQueue(team);
    };
    window.addEventListener('teak-collab-preview-courier', onPreview);
    return () => window.removeEventListener('teak-collab-preview-courier', onPreview);
  }, [team]);

  if (!team) {
    return (
      <div className="collab-office-empty">
        <div className="collab-office-empty-room" aria-hidden="true">
          <span className="collab-office-empty-desk" />
          <span className="collab-office-empty-chair" />
          <span className="collab-office-empty-plant" />
        </div>
        <strong>{t('collab.empty.title')}</strong>
        <p>{t('collab.empty.description')}</p>
        <button type="button" className="collab-primary-btn" onClick={onEdit} disabled={editDisabled}>
          {t('collab.view.editor')}
        </button>
      </div>
    );
  }

  const members = collaborationMembers(team);
  const state = officeState(snapshot, team);
  const pendingTasks = team.pendingTasks;
  const isDormant = !snapshot.enabled || team.paused || team.archived;
  const named = useMemo(() => {
    const map = new Map(members.map(member => [member.id, member]));
    return map;
  }, [members]);

  return (
    <div className="collab-office">
      <div className="collab-office-summary">
        <div className="collab-office-team-copy">
          <div className="collab-office-team-title">
            <span className={`collab-office-state ${state.className}`}>
              <span />{t(state.key)}
            </span>
            <strong>{team.name}</strong>
          </div>
          <span className="collab-office-workspace" title={team.workspace || undefined}>
            {team.workspace || t('collab.team.workspace_placeholder')}
          </span>
        </div>
        <div className="collab-office-summary-actions">
          {(persistence === 'draft' || editingPreview) && (
            <span className="collab-office-draft-badge">
              {editingPreview ? t('collab.office.edit_preview') : t('collab.office.local_draft')}
            </span>
          )}
          <button
            type="button"
            className="collab-office-refresh"
            onClick={onRefresh}
            disabled={refreshing || refreshDisabled || persistence !== 'backend'}
          >
            <span aria-hidden="true">↻</span>
            {refreshing ? t('collab.office.refreshing') : t('collab.office.refresh')}
          </button>
        </div>
      </div>

      <div
        className={`collab-office-scene${isDormant ? ' is-dormant' : ''}`}
        role="img"
        aria-label={t('collab.office.scene_aria', { team: team.name })}
      >
        <div className="collab-office-floor is-wood" aria-hidden="true" />
        <div className="collab-office-floor is-kitchen" aria-hidden="true" />
        <div className="collab-office-floor is-meet" aria-hidden="true" />
        <div className="collab-office-wall-strip" aria-hidden="true">
          <span className="collab-office-window"><i /><i /><i /></span>
        </div>
        <div className="collab-office-library" aria-hidden="true">
          <span /><span /><span /><span /><span />
        </div>
        <div className="collab-office-crates" aria-hidden="true">
          <i /><i />
        </div>
        <div className="collab-office-kitchen-props" aria-hidden="true">
          <span className="collab-office-fridge" />
          <span className="collab-office-coffee-machine"><i /></span>
          <span className="collab-office-clock" />
          <span className="collab-office-bin" />
        </div>
        <div className="collab-office-meet-props" aria-hidden="true">
          <span className="collab-office-painting" />
          <span className="collab-office-bookcase is-meet">
            <span /><span /><span /><span />
          </span>
          <span className="collab-office-meeting-table" />
          <span className="collab-office-meet-chair is-a" />
          <span className="collab-office-meet-chair is-b" />
          <span className="collab-office-meet-plant is-a" /><span className="collab-office-meet-plant is-b" />
        </div>
        <div className="collab-office-plant is-left" aria-hidden="true"><i /><i /><i /></div>
        <div className="collab-office-plant is-right" aria-hidden="true"><i /><i /><i /></div>
        <OfficeSeat
          member={team.leader}
          active={!isDormant}
          statusLabel={t(STATUS_KEY[team.leader.status])}
          away={false}
          receiving={!!courier?.handingOff}
        />
        {team.workers.map((worker, index) => (
          <OfficeSeat
            key={worker.id}
            member={worker}
            active={!isDormant}
            statusLabel={t(STATUS_KEY[worker.status])}
            away={courier?.workerId === worker.id}
            receiving={false}
            workerIndex={index}
            workerCount={team.workers.length}
          />
        ))}
        {courier && (
          <div
            className={`collab-office-courier is-${courier.outcome}${courier.preview ? ' is-preview' : ''}`}
            style={{ left: `${courier.left}%`, top: `${courier.top}%` }}
            aria-hidden="true"
          >
            <PixelAvatar
              avatarId={named.get(courier.workerId)?.avatarId ?? team.workers[0]?.avatarId ?? 'moss'}
              pose="walk"
              facing={courier.facing}
              animated
            />
            {courier.carrying && (
              <span className="collab-office-courier-paper">
                <i />
              </span>
            )}
            <span className="collab-office-courier-caption">
              {t('collab.office.courier.report')}
            </span>
          </div>
        )}
      </div>

      <div className="collab-office-data-grid">
        <section className="collab-office-signals" aria-labelledby="collab-office-signals-title">
          <div className="collab-office-section-heading">
            <span id="collab-office-signals-title">{t('collab.office.signals')}</span>
            <span>{members.length}</span>
          </div>
          <div className="collab-office-signal-list">
            {members.map(member => (
              <div key={member.id} className="collab-office-signal-row">
                <PixelAvatar avatarId={member.avatarId} pose="bust" />
                <div>
                  <strong>{member.displayName || member.alias}</strong>
                  <span>{member.role === 'leader' ? t('collab.member.leader') : t('collab.member.worker')}</span>
                </div>
                <span className={`collab-office-member-state is-${member.status}`}>
                  <i />{t(STATUS_KEY[member.status])}
                </span>
              </div>
            ))}
          </div>
        </section>

        <section className="collab-office-feed" aria-labelledby="collab-office-feed-title">
          <div className="collab-office-section-heading">
            <span id="collab-office-feed-title">{t('collab.office.feed')}</span>
            <span className="collab-office-pending-value">
              {pendingTasks === undefined ? '—' : pendingTasks}
            </span>
          </div>
          {reports && reports.length > 0 ? (
            <ul className="collab-office-report-list">
              {reports.slice(0, 4).map(report => {
                const worker = named.get(report.workerMemberId);
                const name = worker?.displayName || worker?.alias || report.workerMemberId;
                return (
                  <li key={report.id} className={`collab-office-report-item is-${report.outcome}`}>
                    {t(
                      report.outcome === 'failed'
                        ? 'collab.office.feed_report_failed'
                        : 'collab.office.feed_report_completed',
                      { name },
                    )}
                  </li>
                );
              })}
            </ul>
          ) : (
            <div className={`collab-office-feed-card${pendingTasks ? ' has-pending' : ''}`}>
              <span className="collab-office-inbox" aria-hidden="true"><i /></span>
              <div>
                <strong>
                  {pendingTasks === undefined
                    ? t('collab.office.pending_unknown')
                    : pendingTasks === 0
                      ? t('collab.office.feed_empty')
                      : t('collab.office.feed_count', { count: pendingTasks })}
                </strong>
                <p>{t('collab.office.feed_unavailable')}</p>
              </div>
            </div>
          )}
          <p className="collab-office-truth-note">
            {editingPreview || persistence === 'draft'
              ? t('collab.office.preview_note')
              : t('collab.office.truth_note')}
          </p>
        </section>
      </div>
    </div>
  );
}
