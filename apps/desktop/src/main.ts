import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'

const app = mount(App, {
  target: document.getElementById('app')!,
})

// dev-only in-engine probe (see probe.ts) — activated by VITE_PROBE=1
if (import.meta.env.DEV && import.meta.env.VITE_PROBE) {
  void import('./probe').then((m) => m.runProbe())
}

export default app
