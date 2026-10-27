/**
 * 主题（浅/深色）切换：html.dark class + localStorage 持久化
 */
const THEME_KEY = 'yunshu.theme'

export type Theme = 'light' | 'dark'

export function currentTheme(): Theme {
  return document.documentElement.classList.contains('dark') ? 'dark' : 'light'
}

/** 启动时恢复上次选择的主题（默认浅色） */
export function initTheme(): void {
  try {
    if (localStorage.getItem(THEME_KEY) === 'dark') {
      document.documentElement.classList.add('dark')
    }
  } catch {
    /* localStorage 不可用时忽略 */
  }
}

export function toggleTheme(): Theme {
  const next: Theme = currentTheme() === 'dark' ? 'light' : 'dark'
  document.documentElement.classList.toggle('dark', next === 'dark')
  try {
    localStorage.setItem(THEME_KEY, next)
  } catch {
    /* 忽略持久化失败 */
  }
  return next
}
