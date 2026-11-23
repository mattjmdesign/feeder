import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Bar, Window } from '../stageshow/kit/window'
import { Collapse } from '../stageshow/kit/motion/Collapse'
import { Badge } from '../stageshow/kit/controls/Badge'
import { Segmented } from '../stageshow/kit/controls/Segmented'
import { Spinner } from '../stageshow/kit/parts/Spinner'
import { ORBS, orbBackground, type Orb } from '../lib/orb'
import { ToolLogo } from './ToolLogo'
import { useSiteTheme } from './theme'
import './feed-demo.css'

type S = { working: boolean; posted: boolean; slide: number }

const initial: S = { working: false, posted: false, slide: 0 }

async function script(a: Act<S>) {
  await a.wait(650)
  a.mark('working')
  a.put({ working: true, slide: 0 })
  await a.wait(1600)
  a.mark('posted')
  await a.set({ working: false, posted: true, slide: 0 })
  await a.wait(950)
  await a.click('car-next')
  a.mark('slide-2')
  await a.set({ slide: 1 })
  await a.wait(1350)
  await a.click('car-next')
  a.mark('slide-3')
  await a.set({ slide: 2 })
  await a.wait(2200)
  await a.set({ posted: false, slide: 0 })
  await a.wait(700)
}

function OrbDot({ orb, size = 22 }: { orb: Orb; size?: number }) {
  return <span className="fd-orb" style={{ width: size, height: size, background: orbBackground(orb) }} />
}

function SlideWelcome() {
  return (
    <div className="fd-slide fd-slide--welcome">
      <div className="fd-shot-bar">
        <span className="fd-shot-dots"><i /><i /><i /></span>
        <span className="fd-shot-title">Orbit · Step 1 of 3</span>
      </div>
      <div className="fd-shot-welcome">
        <span className="fd-orb fd-shot-orb" style={{ width: 42, height: 42, background: orbBackground(ORBS.orbit!) }} />
        <div className="fd-shot-h">Welcome to Orbit</div>
        <div className="fd-shot-p">Three quick steps to sync your workspace.</div>
        <span className="fd-shot-cta">Continue</span>
      </div>
    </div>
  )
}

function SlideConnect() {
  return (
    <div className="fd-slide fd-slide--connect">
      <div className="fd-shot-bar">
        <span className="fd-shot-dots"><i /><i /><i /></span>
        <span className="fd-shot-title">Orbit · Connect tools</span>
      </div>
      <div className="fd-shot-list">
        <div className="fd-shot-row">
          <span className="fd-shot-tool"><ToolLogo tool="claude" size={13} />Claude Code</span>
          <span className="fd-shot-pill fd-shot-pill--on">Connected</span>
        </div>
        <div className="fd-shot-row">
          <span className="fd-shot-tool"><ToolLogo tool="codex" size={13} />Codex</span>
          <span className="fd-shot-pill fd-shot-pill--on">Connected</span>
        </div>
        <div className="fd-shot-row">
          <span className="fd-shot-tool"><ToolLogo tool="cursor" size={13} />Cursor</span>
          <span className="fd-shot-pill">Connect</span>
        </div>
      </div>
    </div>
  )
}

function SlideReady() {
  return (
    <div className="fd-slide fd-slide--ready">
      <div className="fd-shot-bar">
        <span className="fd-shot-dots"><i /><i /><i /></span>
        <span className="fd-shot-title">Orbit · Ready</span>
      </div>
      <div className="fd-shot-ready">
        <div className="fd-shot-kpis">
          <div className="fd-shot-kpi">
            <span className="fd-shot-kpi__n">3</span>
            <span className="fd-shot-kpi__l">Steps saved</span>
          </div>
          <div className="fd-shot-kpi">
            <span className="fd-shot-kpi__n">42 ms</span>
            <span className="fd-shot-kpi__l">Cold launch</span>
          </div>
          <div className="fd-shot-kpi">
            <span className="fd-shot-kpi__n">100%</span>
            <span className="fd-shot-kpi__l">Local state</span>
          </div>
        </div>
        <div className="fd-shot-banner">
          <span className="fd-shot-banner__dot" />
          Progress saved automatically between launches
        </div>
      </div>
    </div>
  )
}

export default function FeedDemo() {
  const theme = useSiteTheme()

  return (
    <Stage<S>
      width={960}
      height={580}
      compact={{ below: 720, width: 358, height: 400 }}
      theme={theme}
      greys="cool"
      initial={initial}
      poster={{ working: false, posted: true, slide: 0 }}
      script={script}
      cursor={{ at: { wide: { x: 840, y: 470 }, compact: { x: 315, y: 340 } }, style: 'real' }}
      className="fd"
    >
      {(s, _rect, layout) => (
        <Window
          className="fd-win"
          toolbar={layout === 'compact' ? undefined : <Segmented className="fd-seg" items={['Home', 'Feed', 'Notebook']} value={1} />}
        >
          <div className="fd-body">
            <div className="fd-filters">
              <span className="fd-search">
                <Bar w={96} h={6} tone={2} />
              </span>
              <span className="fd-chip" data-on>All</span>
              <span className="fd-chip"><Bar w={54} h={6} tone={2} /></span>
              <span className="fd-chip">Media</span>
            </div>
            <div className="fd-day">
              <span>Today</span>
            </div>

            <div className="fd-list">
              <Collapse open={s.working}>
                <div className="fd-pulse">
                  <OrbDot orb={ORBS.orbit!} size={18} />
                  <span className="fd-pulse__name">Orbit</span>
                  <span className="fd-pulse__what">
                    <Spinner size={10} />
                    Working on onboarding
                  </span>
                  <span className="fd-card__spacer" />
                  <span className="fd-card__when">now</span>
                </div>
              </Collapse>

              <Collapse open={s.posted} reveal="drop">
                <article className="fd-card" data-lead>
                  <header className="fd-card__head">
                    <OrbDot orb={ORBS.orbit!} />
                    <span className="fd-card__project">Orbit</span>
                    <span className="fd-card__agent"><ToolLogo tool="codex" size={12} />Codex</span>
                    <span className="fd-card__spacer" />
                    <Badge tone="warning" dot className="fd-needs">Needs you</Badge>
                    <span className="fd-card__when">just now</span>
                  </header>
                  <div className="fd-block">
                    <p>
                      <strong>Shipped the three-screen onboarding flow</strong> with progress saved between launches.
                      Keep the skip button on screen one, or drop it?
                    </p>
                  </div>

                  <div className="fd-carousel">
                    <div className="fd-carousel__frame">
                      <div className="fd-carousel__track" style={{ transform: `translateX(-${s.slide * 100}%)` }}>
                        <SlideWelcome />
                        <SlideConnect />
                        <SlideReady />
                      </div>
                      <span className="fd-carousel__count">{s.slide + 1} / 3</span>
                      <span className="fd-carousel__arrow fd-carousel__arrow--prev" aria-hidden="true">‹</span>
                      <span
                        className="fd-carousel__arrow fd-carousel__arrow--next"
                        data-target="car-next"
                        data-cursor="pointer"
                        aria-hidden="true"
                      >
                        ›
                      </span>
                    </div>
                    <div className="fd-carousel__dots" aria-hidden="true">
                      {[0, 1, 2].map(i => (
                        <span key={i} className="fd-carousel__dot" data-on={s.slide === i || undefined} />
                      ))}
                    </div>
                  </div>
                </article>
              </Collapse>

              <article className="fd-card">
                <header className="fd-card__head">
                  <OrbDot orb={ORBS.atlas!} />
                  <span className="fd-card__project">Atlas API</span>
                  <span className="fd-card__agent"><ToolLogo tool="claude" size={12} />Claude Code</span>
                  <span className="fd-card__spacer" />
                  <span className="fd-card__when">2h ago</span>
                </header>
                <div className="fd-block">
                  <p>Rate-limit middleware runs before auth now so bad traffic never touches the session table.</p>
                </div>
              </article>
            </div>
          </div>
        </Window>
      )}
    </Stage>
  )
}
