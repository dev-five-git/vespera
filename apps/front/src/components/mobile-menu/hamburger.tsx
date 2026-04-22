'use client'

import { css } from '@devup-ui/react'

import { useSheet } from '../sheet'

export function Hamburger() {
  const { isOpen } = useSheet()
  return (
    <svg
      fill="none"
      height="24"
      viewBox="0 0 24 24"
      width="24"
      xmlns="http://www.w3.org/2000/svg"
    >
      <g clipPath="url(#clip0_81_174)">
        <path
          className={css({
            transform: isOpen ? 'rotate(45deg) translate(6px, 6px)' : null,
            transition: 'transform 0.2s ease-in-out',
            transformOrigin: 'top center',
          })}
          d="M3 5H21"
          stroke="#10131F"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="2"
        />
        <path
          className={css({
            opacity: isOpen ? 0 : 1,
            transition: 'opacity 0.2s ease-in-out',
          })}
          d="M3 12H21"
          stroke="#10131F"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="2"
        />
        <path
          className={css({
            transform: isOpen ? 'rotate(-45deg) translate(6px, -6px)' : null,
            transition: 'transform 0.2s ease-in-out',
            transformOrigin: 'bottom center',
          })}
          d="M3 19H21"
          stroke="#10131F"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="2"
        />
      </g>
      <defs>
        <clipPath id="clip0_81_174">
          <rect fill="white" height="24" width="24" />
        </clipPath>
      </defs>
    </svg>
  )
}
