import type { Metadata } from 'next'
import { notFound } from 'next/navigation'

import { SIDE_MENU_ITEMS, SideMenuItem } from '@/constants'

function getPageNamesFromSideMenuItems(items: SideMenuItem[]): string[] {
  function joinNames(item: SideMenuItem, prefix: string = ''): string[] {
    const name = [...(prefix ? [prefix] : []), item.value].join('.')
    return [
      name,
      ...(item.children?.flatMap((child) => joinNames(child, name)) ?? []),
    ]
  }

  return items.flatMap((item) => joinNames(item))
}

function findLabelFromSegments(
  items: SideMenuItem[],
  segments: string[],
): string | undefined {
  const [head, ...rest] = segments
  const match = items.find((item) => item.value === head)
  if (!match) return undefined
  if (rest.length === 0) return match.label
  return findLabelFromSegments(match.children ?? [], rest)
}

export const dynamicParams = false

export function generateStaticParams() {
  const names = getPageNamesFromSideMenuItems(SIDE_MENU_ITEMS.documentation)
  return names.map((name) => ({ name: name.split('.') }))
}

export async function generateMetadata({
  params,
}: {
  params: Promise<{ name: string[] }>
}): Promise<Metadata> {
  const { name } = await params
  const label =
    findLabelFromSegments(SIDE_MENU_ITEMS.documentation, name) ?? name.join(' ')
  const title = `Vespera - ${label}`
  const description = `${label} · Vespera documentation`
  const url = `/documentation/${name.join('/')}`
  return {
    title,
    description,
    alternates: {
      canonical: url,
    },
    openGraph: {
      title,
      description,
      url,
      siteName: 'Vespera',
    },
  }
}

export default async function Page({
  params,
}: {
  params: Promise<{ name: string[] }>
}) {
  const { name } = await params
  const names = getPageNamesFromSideMenuItems(SIDE_MENU_ITEMS.documentation)
  if (!names.includes(name.join('.'))) notFound()
  const { default: Documentation } = await import(`./${name.join('.')}.mdx`)
  return <Documentation />
}
