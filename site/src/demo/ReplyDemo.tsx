import { Stage } from '../stageshow/kit/stage'
import type { Act } from '../stageshow/kit/act'
import { Collapse } from '../stageshow/kit/motion/Collapse'
import { Badge } from '../stageshow/kit/controls/Badge'
import { Button } from '../stageshow/kit/controls/Button'
import { Field } from '../stageshow/kit/controls/Field'
import { Avatar } from '../stageshow/kit/parts/Avatar'
import { ORBS, orbBackground } from '../lib/orb'
import { ToolLogo } from './ToolLogo'
import { useSiteTheme } from './theme'
import './demos.css'

// Answering an agent from the feed. One update card, cropped from the Feed:
// its last block is a decision, so it carries Needs you. Beats, once in view:
// the cursor clicks the reply field, types the answer, presses Send; the
// reply opens into the card's thread above the field and the tag turns to
// Replied on the same pill. The next agent session reads that reply.
//
// Real strings: the project, the agent, the decision, the reply, the tag.

const ANSWER = 'Ship CSV now. PDF can wait.'

type S = { focused: boolean; draft: string; sent: boolean }

const initial: S = { focused: false, draft: '', sent: false }

async function script(a: Act<S>) {
  await a.wait(700)
  await a.click('reply')
  a.put({ focused: true })
  await a.wait(250)
  await a.type('draft', ANSWER)
  await a.wait(350)
  await a.click({ at: 'send', x: 0.85, y: 0.75 })
  a.mark('sent')
  a.put({ sent: true, draft: '', focused: false })
  await a.wait(1600)
}

export default function ReplyDemo() {
  const theme = useSiteTheme()
  return (
    <Stage<S>
      width={620}
      height={340}
      compact={{ below: 560, width: 358, height: 340 }}
      fill
      theme={theme}
      greys="cool"
      play="once"
      initial={initial}
      poster={{ focused: false, draft: '', sent: true }}
      script={script}
      cursor={{ at: { wide: { x: 560, y: 300 }, compact: { x: 320, y: 300 } }, style: 'real' }}
      className="rd"
    >
      {s => (
        <article className="rd-card">
          <header className="rd-head">
            <span className="demo-orb" style={{ width: 22, height: 22, background: orbBackground(ORBS.ledger!) }} />
            <span className="rd-project">Ledger</span>
            <span className="rd-agent"><ToolLogo tool="claude" size={12} />Claude Code</span>
            <span className="rd-spacer" />
            <span className="demo-fade">
              <Badge tone="warning" dot data-on={!s.sent || undefined}>Needs you</Badge>
              <Badge tone="success" dot data-on={s.sent || undefined}>Replied</Badge>
            </span>
          </header>
          <div className="rd-label">Decision needed</div>
          <p className="rd-text">CSV export works. Ship it now, or wait until PDF export is done too?</p>
          <Collapse open={s.sent} reveal="drop">
            <div className="rd-reply">
              <Avatar name="You" size={22} />
              <span className="rd-reply__who">You</span>
              <span className="rd-reply__text">{ANSWER}</span>
            </div>
          </Collapse>
          <div className="rd-compose">
            <Field className="rd-field" value={s.draft} placeholder="Reply to Claude Code" focused={s.focused} box={{ 'data-target': 'reply', 'data-cursor': 'ibeam' }} />
            <Button variant="primary" data-target="send" data-cursor="pointer">
              Send
            </Button>
          </div>
        </article>
      )}
    </Stage>
  )
}
