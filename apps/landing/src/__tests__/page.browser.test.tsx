import { ThemeScript } from '@devup-ui/react'
import { describe, expect, it } from 'bun:test'
import { render } from 'bun-test-env-dom'

import HomePage from '@/app/page'
import { HeaderProvider } from '@/components/header/header-provider'
import { SheetRouter } from '@/components/sheet/router'

// `HomePage` renders `HeaderSentinel`, which calls `useHeader()`. In the app
// that context comes from `app/layout.tsx`; a bare `render(<HomePage />)`
// therefore throws "useHeader must be used within a HeaderProvider". Mirror the
// minimal slice of the layout stack HomePage actually depends on:
// `SheetRouter` (required by `HeaderProvider`) → `HeaderProvider`.
function renderHomePage() {
  return render(
    <SheetRouter>
      <HeaderProvider>
        <HomePage />
      </HeaderProvider>
    </SheetRouter>,
  )
}

describe('ThemeScript', () => {
  it('should render the theme bootstrap script', () => {
    const { container } = render(<ThemeScript />)
    expect(container).toMatchSnapshot()
  })
})

describe('HomePage', () => {
  it('should render its headline sections', () => {
    const { getByText } = renderHomePage()

    expect(getByText(/documented Rust APIs\./)).toBeTruthy()
    expect(getByText('FastAPI-grade DX, Rust-grade performance')).toBeTruthy()
    expect(getByText('Zero to documented API in three steps')).toBeTruthy()
    expect(getByText('Join our community')).toBeTruthy()
  })

  it('should compile devup-ui style props away into class names', () => {
    const { container } = renderHomePage()

    // The devup-ui bun plugin (preloaded via bunfig.toml) rewrites style props
    // such as `bg="#0A0E1A"` into extracted class names. Without it every
    // `@devup-ui/react` component throws "Cannot run on the runtime", so a
    // rendered root element with classes is the regression guard for the
    // build-time transform staying wired up in tests.
    const root = container.firstElementChild
    expect(root).toBeTruthy()
    expect(root?.tagName).toBe('DIV')
    expect(root?.className.length).toBeGreaterThan(0)
    expect(container.querySelectorAll('[style*="Cannot run"]').length).toBe(0)
  })
})
