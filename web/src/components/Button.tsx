import type { ButtonHTMLAttributes } from 'react';
import { Link, type LinkProps } from 'react-router-dom';

type Variant = 'primary' | 'secondary';

const base =
  'inline-flex items-center justify-center gap-2 rounded-xl px-5 py-2.5 text-sm font-semibold transition-[transform,background-color,box-shadow] duration-150 ease-out active:scale-[0.97] disabled:opacity-50 disabled:pointer-events-none cursor-pointer select-none';
const variants: Record<Variant, string> = {
  primary: 'bg-ink text-white hover:bg-[#2c2e35] shadow-[0_1px_2px_rgb(23_24_28/0.25)]',
  secondary: 'bg-surface text-ink border border-line-strong hover:bg-sunken',
};

export const buttonClass = (variant: Variant = 'primary', extra = '') =>
  `${base} ${variants[variant]} ${extra}`;

export function Button({
  variant = 'primary',
  className = '',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return <button type="button" className={buttonClass(variant, className)} {...props} />;
}

export function ButtonLink({
  variant = 'secondary',
  className = '',
  ...props
}: LinkProps & { variant?: Variant }) {
  return <Link className={buttonClass(variant, className)} {...props} />;
}
