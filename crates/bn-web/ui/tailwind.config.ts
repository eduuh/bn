import type { Config } from "tailwindcss";
import animate from "tailwindcss-animate";

/**
 * Tailwind for the bn-web dashboard (#554). **Preflight is disabled on purpose**: only two screens
 * migrate to Tailwind + shadcn in Phase 1 while ~19 others still render off the hand-rolled
 * `src/index.css`. Tailwind's global reset would clobber those un-migrated screens (heading sizes,
 * list markers, button chrome), so we opt out of preflight and re-add just the border reset shadcn
 * needs inside `@layer base` in `tailwind.css`. All colours are CSS-variable tokens (see
 * `tailwind.css`) so light/dark and the semantic urgent/warn/ok/info scale live in one place.
 */
const config: Config = {
  darkMode: ["class"],
  // Scope content to the migrated surfaces + shared UI kit to keep the generated CSS lean.
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    container: {
      center: true,
      padding: "1rem",
    },
    extend: {
      screens: {
        // The console's third column. Tailwind's `xl` (1280px) is too late — the prototype folds
        // Act under Focus at 1180px, which is where 19.5 + 18.5rem of fixed columns stop leaving
        // the focus pane a readable measure.
        wide: "1181px",
      },
      colors: {
        border: "hsl(var(--border))",
        input: "hsl(var(--input))",
        ring: "hsl(var(--ring))",
        background: "hsl(var(--background))",
        foreground: "hsl(var(--foreground))",
        primary: {
          DEFAULT: "hsl(var(--primary))",
          foreground: "hsl(var(--primary-foreground))",
        },
        secondary: {
          DEFAULT: "hsl(var(--secondary))",
          foreground: "hsl(var(--secondary-foreground))",
        },
        destructive: {
          DEFAULT: "hsl(var(--destructive))",
          foreground: "hsl(var(--destructive-foreground))",
        },
        muted: {
          DEFAULT: "hsl(var(--muted))",
          foreground: "hsl(var(--muted-foreground))",
        },
        accent: {
          DEFAULT: "hsl(var(--accent))",
          foreground: "hsl(var(--accent-foreground))",
        },
        popover: {
          DEFAULT: "hsl(var(--popover))",
          foreground: "hsl(var(--popover-foreground))",
        },
        card: {
          DEFAULT: "hsl(var(--card))",
          foreground: "hsl(var(--card-foreground))",
        },
        // Semantic dashboard tokens: the four signal tones the fleet/action rows key off.
        urgent: {
          DEFAULT: "hsl(var(--urgent))",
          foreground: "hsl(var(--urgent-foreground))",
          muted: "hsl(var(--urgent) / 0.14)",
        },
        warn: {
          DEFAULT: "hsl(var(--warn))",
          foreground: "hsl(var(--warn-foreground))",
          muted: "hsl(var(--warn) / 0.14)",
        },
        ok: {
          DEFAULT: "hsl(var(--ok))",
          foreground: "hsl(var(--ok-foreground))",
          muted: "hsl(var(--ok) / 0.14)",
        },
        info: {
          DEFAULT: "hsl(var(--info))",
          foreground: "hsl(var(--info-foreground))",
          muted: "hsl(var(--info) / 0.14)",
        },
      },
      borderRadius: {
        lg: "var(--radius)",
        md: "calc(var(--radius) - 2px)",
        sm: "calc(var(--radius) - 4px)",
      },
      fontFamily: {
        sans: "var(--font-sans)",
        display: "var(--font-display)",
        mono: "var(--font-mono)",
      },
      boxShadow: {
        card: "0 1px 2px rgba(0,0,0,0.28)",
        pop: "0 16px 48px rgba(0,0,0,0.5)",
      },
      keyframes: {
        "accordion-down": {
          from: { height: "0" },
          to: { height: "var(--radix-accordion-content-height)" },
        },
        "accordion-up": {
          from: { height: "var(--radix-accordion-content-height)" },
          to: { height: "0" },
        },
      },
      animation: {
        "accordion-down": "accordion-down 0.2s ease-out",
        "accordion-up": "accordion-up 0.2s ease-out",
      },
    },
  },
  plugins: [animate],
};

export default config;
