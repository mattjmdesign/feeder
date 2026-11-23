import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Bar, Window } from '../stageshow/kit/window'
import { Count } from '../stageshow/kit/motion/Count'
import { Segmented } from '../stageshow/kit/controls/Segmented'
import { Spinner } from '../stageshow/kit/parts/Spinner'
import { ORBS, orbAccent, orbBackground, type Orb } from '../lib/orb'
import { ToolLogo, type ToolKey } from './ToolLogo'
import { useSiteTheme } from './theme'
import './demos.css'

type S = { working: boolean; needs: boolean }

const initial: S = { working: false, needs: false }

async function script(a: Act<S>) {
  await a.wait(700)
  a.mark('working')
  a.put({ working: true })
  await a.wait(1900)
  a.mark('needs')
  a.put({ needs: true })
  await a.wait(1200)
}

type Project = { key: string; name: string; orb: Orb; agent?: string; tool?: ToolKey }

const PROJECTS: Project[] = [
  { key: 'orbit', name: 'Orbit', orb: ORBS.orbit!, agent: 'Codex', tool: 'codex' },
  { key: 'atlas', name: 'Atlas API', orb: ORBS.atlas! },
  { key: 'lumen', name: 'Lumen', orb: ORBS.lumen!, agent: 'Cursor', tool: 'cursor' },
  { key: 'ledger', name: 'Ledger', orb: ORBS.ledger! },
  { key: 'harbor', name: 'Harbor', orb: ORBS.harbor! },
]

function Tile({ p, s, size }: { p: Project; s: S; size: number }) {
  const working = p.key === 'orbit' && s.working
  const needs = p.key === 'lumen' && s.needs
  return (
    <div className="hd-tile" style={{ width: size + 40 }}>
      <span className="hd-orb" data-working={working || undefined} style={{ width: size, height: size, ['--halo' as string]: orbAccent(p.orb, 0.55) }}>
        <span className="hd-orb__ball" style={{ background: orbBackground(p.orb) }} />
      </span>
      <span className="hd-name">
        <i style={{ background: orbAccent(p.orb) }} />
        {p.name}
      </span>
      <span className="hd-meta">
        {p.agent && p.tool ? (
          <>
            <ToolLogo tool={p.tool} size={11} />
            <span>{p.agent} · {p.key === 'orbit' ? 'now' : '4m'}</span>
          </>
        ) : (
          <Bar w={58} h={6} tone={2} />
        )}
      </span>
      <span className="hd-status">
        {p.key === 'orbit' && (
          <span className="hd-working" data-on={working || undefined}>
            <Spinner size={9} />
            Writing tests
          </span>
        )}
        {p.key === 'lumen' && (
          <span className="hd-needs" data-on={needs || undefined}>
            Needs you
          </span>
        )}
      </span>
    </div>
  )
}

export default function HomeDemo() {
  const theme = useSiteTheme()
  return (
    <Stage<S>
      width={960}
      height={440}
      compact={{ below: 720, width: 358, height: 330 }}
      theme={theme}
      greys="cool"
      play="once"
      initial={initial}
      poster={{ working: true, needs: true }}
      script={script}
      className="hd"
    >
      {(s, _rect, layout) => {
        const compact = layout === 'compact'
        const list = compact ? PROJECTS.slice(0, 3) : PROJECTS
        return (
          <Window className="hd-win" toolbar={compact ? undefined : <Segmented className="demo-seg" items={['Home', 'Feed', 'Notebook']} value={0} />}>
            <div className="hd-body">
              <div className="hd-filters">
                <span className="demo-chip" data-on>All</span>
                <span className="demo-chip">
                  <i className="hd-dot" data-tone="needs" />
                  Needs you <Count value={s.needs ? 1 : 0} />
                </span>
                <span className="demo-chip">
                  <i className="hd-dot" data-tone="working" />
                  Working <Count value={s.working ? 1 : 0} />
                </span>
              </div>
              <div className="hd-row">
                {list.map(p => (
                  <Tile key={p.key} p={p} s={s} size={compact ? 64 : 104} />
                ))}
              </div>
            </div>
          </Window>
        )
      }}
    </Stage>
  )
}
