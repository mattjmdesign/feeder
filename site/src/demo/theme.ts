import { useEffect, useState } from 'react'

function resolveTheme(): 'light' | 'dark' {
  if (typeof document === 'undefined') return 'dark'
  const explicit = document.documentElement.dataset.theme
  if (explicit === 'light' || explicit === 'dark') return explicit
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export function useSiteTheme(): 'light' | 'dark' {
  const [theme, setTheme] = useState<'light' | 'dark'>(resolveTheme)

  useEffect(() => {
    const sync = () => setTheme(resolveTheme())
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    mq.addEventListener('change', sync)
    const mo = new MutationObserver(sync)
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
    return () => {
      mq.removeEventListener('change', sync)
      mo.disconnect()
    }
  }, [])

  return theme
}
