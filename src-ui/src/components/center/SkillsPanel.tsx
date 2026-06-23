import { useEffect, useState, useCallback, useLayoutEffect, useRef, type WheelEvent as ReactWheelEvent } from 'react';
import { createPortal } from 'react-dom';
import { convertFileSrc } from '@tauri-apps/api/core';
import { commands } from '../../tauri';
import type { Marketplace } from '../../tauri';
import { useAppState } from '../../store/app-state';
import { parseFrontmatter, localizedField } from '../../utils/skill-meta';

// Unified card model — a bundled skill OR a marketplace plugin render
// identically. `kind` only decides which toggle command to call.
interface DisplayCard {
  key: string;
  displayName: string;
  description: string;
  /** Ready-to-use <img src>: a data: URL (bundled) or asset:// (plugin). */
  iconSrc: string | null;
  enabled: boolean;
  kind: 'bundled' | 'plugin';
}

const BUILTIN_TAB = '__builtin__';

// Module-scoped cache so re-opening the Skills tab paints instantly while a
// fresh list loads in the background — no skeleton flash on every re-open.
let cachedBundled: DisplayCard[] | null = null;
let cachedMarkets: Marketplace[] | null = null;

interface Props {
  /** Top-of-screen iOS-style toast — owned by CenterPanel, wired through
   *  here so toggle confirmations slot into the existing animation pipe. */
  showToast: (msg: string) => void;
}

export function SkillsPanel({ showToast }: Props) {
  const { state } = useAppState();
  const lang = state.currentLang;
  const zh = lang.startsWith('zh');
  // Niche power-user feature — inline zh/en instead of 11-locale i18n keys.
  const L = {
    builtin: zh ? '内置' : 'Built-in',
    add: zh ? '添加技能市场' : 'Add marketplace',
    manage: zh ? '管理' : 'Manage',
    addTitle: zh ? '添加技能市场' : 'Add skill marketplace',
    addHint: zh
      ? '兼容 Codex 插件市场规则:仓库需含 .agents/plugins/marketplace.json'
      : 'Codex-compatible: the repo must contain .agents/plugins/marketplace.json',
    addPlaceholder: 'https://github.com/openai/plugins.git',
    cancel: zh ? '取消' : 'Cancel',
    confirm: zh ? '添加' : 'Add',
    adding: zh ? '克隆中…' : 'Cloning…',
    empty: zh ? '这个市场暂无可显示的插件。' : 'No plugins to show here.',
    none: zh ? '暂无技能。' : 'No skills available yet.',
    manageTitle: zh ? '管理技能市场' : 'Manage marketplaces',
    upgrade: zh ? '升级' : 'Upgrade',
    upgrading: zh ? '升级中…' : 'Upgrading…',
    pluginsN: zh ? '个插件' : 'plugins',
    noMarkets: zh ? '还没有添加任何市场。' : 'No marketplaces added yet.',
    openDir: zh ? '打开目录' : 'Open folder',
    close: zh ? '关闭' : 'Close',
  };

  const [bundled, setBundled] = useState<DisplayCard[]>(() => cachedBundled ?? []);
  const [marketplaces, setMarketplaces] = useState<Marketplace[]>(() => cachedMarkets ?? []);
  const [activeTab, setActiveTab] = useState<string>(BUILTIN_TAB);
  // Skeleton only on the very first load (no cache). Re-opens paint cached
  // cards immediately and reconcile via refresh() in the background.
  const [loading, setLoading] = useState(cachedBundled === null);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  // Progressive render — big markets (Codex official ≈178 plugins) jank if
  // every card mounts at once. Render PAGE, bump as a sentinel scrolls in.
  const PAGE = 30;
  const [visibleCount, setVisibleCount] = useState(PAGE);
  const sentinelRef = useRef<HTMLDivElement | null>(null);

  // ── Tab strip: vertical wheel scrolls the row sideways (desktop mice have
  //    no horizontal wheel, so without this extra market tabs are unreachable). ──
  const tabsRef = useRef<HTMLDivElement | null>(null);
  const onTabsWheel = useCallback((e: ReactWheelEvent<HTMLDivElement>) => {
    const el = tabsRef.current;
    if (!el || el.scrollWidth <= el.clientWidth || e.deltaY === 0) return;
    el.scrollLeft += e.deltaY;
  }, []);

  // ── Add-marketplace modal ──
  const [addOpen, setAddOpen] = useState(false);
  const [addUrl, setAddUrl] = useState('');
  const [adding, setAdding] = useState(false);
  // ── Manage-marketplaces modal ──
  const [manageOpen, setManageOpen] = useState(false);
  const [busyMarket, setBusyMarket] = useState<string | null>(null);

  // ── Mouse-tracked description tooltip (portaled, viewport-clamped) ──
  const [tip, setTip] = useState<{ x: number; y: number; text: string } | null>(null);
  const tipRef = useRef<HTMLDivElement | null>(null);
  useLayoutEffect(() => {
    if (!tip || !tipRef.current) return;
    const el = tipRef.current;
    const rect = el.getBoundingClientRect();
    const margin = 8;
    let left = tip.x + 12;
    let top = tip.y + 16;
    if (left + rect.width > window.innerWidth - margin) left = Math.max(margin, tip.x - rect.width - 12);
    if (top + rect.height > window.innerHeight - margin) top = Math.max(margin, tip.y - rect.height - 16);
    el.style.left = `${left}px`;
    el.style.top = `${top}px`;
  }, [tip]);
  const handleTipMove = (e: React.MouseEvent, text: string) => { if (text) setTip({ x: e.clientX, y: e.clientY, text }); };
  const handleTipLeave = () => setTip(null);

  const refresh = useCallback(async () => {
    try {
      await commands.skillsEnsureDirs();
      const [raw, markets] = await Promise.all([
        commands.skillsList(),
        commands.listMarketplaces().catch(() => [] as Marketplace[]),
      ]);
      const nextBundled: DisplayCard[] = raw.map(s => {
        const fm = parseFrontmatter(s.skillMd);
        return {
          key: s.name,
          displayName: localizedField(fm, 'name', lang) || s.name,
          description: localizedField(fm, 'description', lang),
          iconSrc: s.iconDataUrl,
          enabled: s.enabled,
          kind: 'bundled' as const,
        };
      });
      cachedBundled = nextBundled;
      cachedMarkets = markets;
      setBundled(nextBundled);
      setMarketplaces(markets);
    } catch (e) {
      showToast(`Skills load failed: ${e}`);
    } finally {
      setLoading(false);
    }
  }, [showToast, lang]);

  useEffect(() => { refresh(); }, [refresh]);

  const toggleCard = async (card: DisplayCard) => {
    if (busyKey) return;
    setBusyKey(card.key);
    try {
      const turningOn = !card.enabled;
      if (card.kind === 'bundled') {
        await commands.skillsToggle(card.key, turningOn);
      } else {
        await commands.setMarketplacePluginEnabled(card.key, turningOn);
      }
      await refresh();
    } catch (e) {
      showToast(`Toggle failed: ${e}`);
    } finally {
      setBusyKey(null);
    }
  };

  const handleAdd = async () => {
    const url = addUrl.trim();
    if (!url || adding) return;
    setAdding(true);
    try {
      await commands.addMarketplace(url);
      setAddOpen(false);
      setAddUrl('');
      await refresh();
    } catch (e) {
      showToast(`${e}`);
    } finally {
      setAdding(false);
    }
  };

  const upgradeMarket = async (id: string) => {
    if (busyMarket) return;
    setBusyMarket(id);
    try { await commands.updateMarketplace(id); await refresh(); }
    catch (e) { showToast(`${e}`); }
    finally { setBusyMarket(null); }
  };

  const deleteMarket = async (id: string) => {
    if (busyMarket) return;
    setBusyMarket(id);
    try {
      await commands.deleteMarketplace(id);
      if (activeTab === id) setActiveTab(BUILTIN_TAB);
      await refresh();
    } catch (e) { showToast(`${e}`); }
    finally { setBusyMarket(null); }
  };

  // Cards for the active tab.
  const activeMarket = marketplaces.find(m => m.id === activeTab);
  const cards: DisplayCard[] = activeTab === BUILTIN_TAB
    ? bundled
    : (activeMarket?.plugins ?? []).map(p => ({
        key: p.key,
        displayName: p.displayName,
        description: p.description,
        // Asset protocol — browser loads + caches + lazy-decodes; no base64.
        iconSrc: p.iconPath ? convertFileSrc(p.iconPath) : null,
        enabled: p.enabled,
        kind: 'plugin' as const,
      }));
  const visibleCards = cards.slice(0, visibleCount);
  const hasMore = cards.length > visibleCount;

  // Reset the render window when switching tabs.
  useEffect(() => { setVisibleCount(PAGE); }, [activeTab]);
  // Bump the window as the bottom sentinel scrolls into view.
  useEffect(() => {
    if (!hasMore) return;
    const el = sentinelRef.current;
    if (!el) return;
    const io = new IntersectionObserver(
      entries => { if (entries.some(e => e.isIntersecting)) setVisibleCount(c => c + PAGE); },
      { rootMargin: '300px' },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hasMore]);

  return (
    <>
      <div className="skills-header">
        <div className="skills-tabs" ref={tabsRef} onWheel={onTabsWheel}>
          <button
            className={`skills-tab ${activeTab === BUILTIN_TAB ? 'is-active' : ''}`}
            onClick={() => setActiveTab(BUILTIN_TAB)}
          >{L.builtin}</button>
          {marketplaces.map(m => (
            <button
              key={m.id}
              className={`skills-tab ${activeTab === m.id ? 'is-active' : ''}`}
              onClick={() => setActiveTab(m.id)}
            >{m.displayName}</button>
          ))}
        </div>
        <div className="skills-header-actions">
          <button className="skills-link-btn" onClick={() => setAddOpen(true)}>[{L.add}]</button>
          <button className="skills-link-btn" onClick={() => setManageOpen(true)}>[{L.manage}]</button>
        </div>
      </div>

      {loading ? (
        <div className="skills-grid">
          {Array.from({ length: 6 }).map((_, i) => (
            <div key={`skel-${i}`} className="skills-card skills-card-skeleton" aria-hidden="true">
              <span className="skills-skel-icon" />
              <div className="skills-card-text">
                <span className="skills-skel-bar skills-skel-title" />
                <span className="skills-skel-bar skills-skel-desc" />
              </div>
            </div>
          ))}
        </div>
      ) : cards.length === 0 ? (
        <div className="skills-empty">{activeTab === BUILTIN_TAB ? L.none : L.empty}</div>
      ) : (
        <div className="skills-grid">
          {visibleCards.map(card => (
            <div key={card.key} className="skills-card">
              <div className="skills-card-icon">
                {card.iconSrc
                  ? <img src={card.iconSrc} alt="" loading="lazy" onError={(e) => { e.currentTarget.style.visibility = 'hidden'; }} />
                  : <span className="skills-card-icon-fallback">{card.displayName.slice(0, 1).toUpperCase()}</span>}
              </div>
              <div
                className="skills-card-text"
                onMouseEnter={(e) => handleTipMove(e, card.description || '')}
                onMouseMove={(e) => handleTipMove(e, card.description || '')}
                onMouseLeave={handleTipLeave}
              >
                <div className="skills-card-name">{card.displayName}</div>
                {card.description && <div className="skills-card-desc">{card.description}</div>}
              </div>
              <button
                className={`skills-toggle ${card.enabled ? 'on' : 'off'} ${busyKey === card.key ? 'is-busy' : ''}`}
                onClick={() => toggleCard(card)}
                disabled={busyKey === card.key}
                aria-label={card.enabled ? 'Disable' : 'Enable'}
              >
                <span className="skills-toggle-track"><span className="skills-toggle-thumb" /></span>
              </button>
            </div>
          ))}
          {hasMore && <div ref={sentinelRef} className="skills-sentinel" aria-hidden="true" />}
        </div>
      )}

      {addOpen && createPortal(
        <div className="skills-modal-backdrop" onMouseDown={() => !adding && setAddOpen(false)}>
          <div className="skills-modal" onMouseDown={(e) => e.stopPropagation()}>
            <div className="skills-modal-title">{L.addTitle}</div>
            <div className="skills-modal-hint">{L.addHint}</div>
            <input
              className="skills-modal-input"
              value={addUrl}
              onChange={(e) => setAddUrl(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') handleAdd(); if (e.key === 'Escape' && !adding) setAddOpen(false); }}
              placeholder={L.addPlaceholder}
              autoFocus
              spellCheck={false}
              disabled={adding}
            />
            <div className="skills-modal-actions">
              <button className="skills-modal-btn" onClick={() => setAddOpen(false)} disabled={adding}>{L.cancel}</button>
              <button className="skills-modal-btn primary" onClick={handleAdd} disabled={adding || !addUrl.trim()}>
                {adding ? L.adding : L.confirm}
              </button>
            </div>
          </div>
        </div>,
        document.body,
      )}

      {manageOpen && createPortal(
        <div className="skills-modal-backdrop" onMouseDown={() => setManageOpen(false)}>
          <div className="skills-modal skills-manage" onMouseDown={(e) => e.stopPropagation()}>
            <div className="skills-modal-title">{L.manageTitle}</div>
            {marketplaces.length === 0 ? (
              <div className="skills-modal-hint">{L.noMarkets}</div>
            ) : (
              <div className="skills-manage-list">
                {marketplaces.map(m => (
                  <div key={m.id} className="skills-manage-row">
                    <div className="skills-manage-info">
                      <div className="skills-manage-titlerow">
                        <span className="skills-manage-name">{m.displayName}</span>
                        <span className="skills-manage-meta">{m.plugins.length} {L.pluginsN}</span>
                      </div>
                      <div className="skills-manage-path">{m.manifestPath}</div>
                    </div>
                    <div className="skills-manage-actions">
                      <button className="skills-modal-btn" disabled={busyMarket === m.id} onClick={() => upgradeMarket(m.id)}>
                        {busyMarket === m.id ? L.upgrading : L.upgrade}
                      </button>
                      <button className="skills-manage-del" disabled={busyMarket === m.id} onClick={() => deleteMarket(m.id)} aria-label="Delete">
                        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                          <path d="M3 6h18" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                        </svg>
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
            <div className="skills-modal-actions">
              <button className="skills-link-btn" onClick={() => commands.openMarketplaceDir().catch(() => {})}>{L.openDir}</button>
              <button className="skills-modal-btn" onClick={() => setManageOpen(false)}>{L.close}</button>
            </div>
          </div>
        </div>,
        document.body,
      )}

      {tip && createPortal(
        <div ref={tipRef} className="skills-tooltip" style={{ left: tip.x + 12, top: tip.y + 16 }}>{tip.text}</div>,
        document.body,
      )}
    </>
  );
}
