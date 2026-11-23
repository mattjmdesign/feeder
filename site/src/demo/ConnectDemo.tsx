import { Check } from 'lucide-react'
import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Bar, Window } from '../stageshow/kit/window'
import { Button } from '../stageshow/kit/controls/Button'
import { ToolLogo, type ToolKey } from './ToolLogo'
import { useSiteTheme } from './theme'
import './demos.css'

// Connecting a tool in Settings → Connected Tools. Beats, once in view: the cursor
// goes to Codex's Connect and clicks it; the button settles as Connected
// with a check, on the same button, and the row's note changes to say the
// tool needs a restart. Claude Code is already connected, Cursor not yet.
//
// Real strings: the pane title, three tool names, their notes and buttons.

type S = { codex: boolean }

const initial: S = { codex: false }

async function script(a: Act<S>) {
  await a.wait(700)
  await a.click('connect-codex')
  a.mark('connected')
  a.put({ codex: true })
  await a.wait(1600)
}

function ToolRow({
  tool,
  name,
  note,
  after,
  connected,
  target,
}: {
  tool: ToolKey
  name: string
  note: string
  after?: string
  connected: boolean
  target?: string
}) {
  return (
    <div className="cd-row">
      <div className="cd-who">
        <span className="cd-icon">
          <ToolLogo tool={tool} size={18} />
        </span>
        <span className="cd-lines">
          <span className="cd-name">{name}</span>
          <span className="cd-note demo-fade demo-fade--start">
            <span data-on={!after || !connected || undefined}>{note}</span>
            {after && <span data-on={connected || undefined}>{after}</span>}
          </span>
        </span>
      </div>
      <Button
        size="small"
        variant={connected ? 'secondary' : 'primary'}
        icon={connected ? <Check size={12} strokeWidth={2.4} /> : undefined}
        data-target={target}
        data-cursor={target ? 'pointer' : undefined}
      >
        {connected ? 'Connected' : 'Connect'}
      </Button>
    </div>
  )
}

export default function ConnectDemo() {
  const theme = useSiteTheme()
  return (
    <Stage<S>
      width={560}
      height={360}
      compact={{ below: 420, width: 358, height: 320 }}
      theme={theme}
      greys="cool"
      play="once"
      initial={initial}
      poster={{ codex: true }}
      script={script}
      cursor={{ at: { wide: { x: 300, y: 330 }, compact: { x: 200, y: 300 } }, style: 'real' }}
      className="cd"
    >
      {s => (
        <Window className="cd-win" title="Settings">
          <div className="cd-body">
            <div className="cd-title">Connected Tools</div>
            <div className="cd-sub">Connect the tools you use. Restart a tool after connecting it.</div>
            <div className="cd-list">
              <ToolRow tool="claude" name="Claude Code" note="Connected" connected />
              <ToolRow tool="codex" name="Codex" note="Installed" after="Connected · restart Codex" connected={s.codex} target="connect-codex" />
              <ToolRow tool="cursor" name="Cursor" note="Installed" connected={false} />
              <div className="cd-row cd-row--filler">
                <span className="cd-lines">
                  <Bar w={72} h={7} tone={2} />
                  <Bar w={48} h={6} tone={1} />
                </span>
                <Bar w={70} h={26} r={7} tone={1} />
              </div>
            </div>
          </div>
        </Window>
      )}
    </Stage>
  )
}
