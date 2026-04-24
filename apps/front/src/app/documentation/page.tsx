import type { Metadata } from 'next'
import { redirect } from 'next/navigation'

export const metadata: Metadata = {
  title: 'Vespera - Documentation',
  description: 'Vespera documentation',
  alternates: {
    canonical: '/documentation',
  },
  openGraph: {
    title: 'Vespera - Documentation',
    description: 'Vespera documentation',
    url: '/documentation',
    siteName: 'Vespera',
  },
}

export default function Page() {
  redirect('/documentation/overview')
}
