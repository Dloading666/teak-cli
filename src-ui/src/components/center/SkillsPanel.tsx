import { useEffect, useState, useCallback, useLayoutEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
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
  iconDataUrl: string | null;
  enabled: boolean;
  kind: 'bundled' | 'plugin';
}

const BUILTIN_TAB = '__builtin__';

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
    addPlaceholder: 'https://cnb.cool/echobird/codex-wps.git',
    cancel: zh ? '取消' : 'Cancel',
    confirm: zh ? '添加' : 'Add',
    adding: zh ? '克隆中…' : 'Cloning…',
    empty: zh ? '这个市场暂无可显示的插件。' : 'No plugins to show here.',
    none: zh ? '暂无技能。' : 'No skills available yet.',
  };

  const [bundled, setBundled] = useState<DisplayCard[]>([]);
  const [marketplaces, setMarketplaces] = useState<Marketplace[]>([]);
  const [activeTab, setActiveTab] = useState<string>(BUILTIN_TAB);
  const [loading, setLoading] = useState(true);
  const [busyKey, setBusyKey] = useState<string | null>(null);

  // ── Add-marketplace modal ──
  const [addOpen, setAddOpen] = useState(false);
  const [addUrl, setAddUrl] = useState('');
  const [adding, setAdding] = useState(false);

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
      setBundled(
        raw.map(s => {
          const fm = parseFrontmatter(s.skillMd);
          return {
            key: s.name,
            displayName: localizedField(fm, 'name', lang) || s.name,
            description: localizedField(fm, 'description', lang),
            iconDataUrl: s.iconDataUrl,
            enabled: s.enabled,
            kind: 'bundled' as const,
          };
        }),
      );
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

  // Cards for the active tab.
  const activeMarket = marketplaces.find(m => m.id === activeTab);
  const cards: DisplayCard[] = activeTab === BUILTIN_TAB
    ? bundled
    : (activeMarket?.plugins ?? []).map(p => ({
        key: p.key,
        displayName: p.displayName,
        description: p.description,
        iconDataUrl: p.iconDataUrl,
        enabled: p.enabled,
        kind: 'plugin' as const,
      }));

  return (
    <>
      <div className="skills-header">
        <div className="skills-tabs">
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
          <button className="skills-link-btn" onClick={() => commands.openMarketplaceDir().catch(() => {})}>[{L.manage}]</button>
        </div>
      </div>

      {loading ? (
        <div className="skills-empty">Loading…</div>
      ) : cards.length === 0 ? (
        <div className="skills-empty">{activeTab === BUILTIN_TAB ? L.none : L.empty}</div>
      ) : (
        <div className="skills-grid">
          {cards.map(card => (
            <div key={card.key} className="skills-card">
              <div className="skills-card-icon">
                {card.iconDataUrl
                  ? <img src={card.iconDataUrl} alt="" />
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

      {tip && createPortal(
        <div ref={tipRef} className="skills-tooltip" style={{ left: tip.x + 12, top: tip.y + 16 }}>{tip.text}</div>,
        document.body,
      )}
    </>
  );
}
