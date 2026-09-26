import type { MDXComponents } from "mdx/types";
import Link from "next/link";
import { CalloutBox } from "@/components/CalloutBox";
import { CodeBlock } from "@/components/CodeBlock";
import { MultiCodeBlock } from "@/components/MultiCodeBlock";
import { EventExplorer } from "@/components/EventExplorer";
import { ApiEndpoint } from "@/components/ApiEndpoint";
import { ApiPlayground } from "@/components/ApiPlayground";
import { CapTableVisualizer } from "@/components/CapTableVisualizer";
import { ProspectusViewer } from "@/components/ProspectusViewer";
import { LazyProspectusViewer } from "@/components/LazyProspectusViewer";
import { SorobanIDE } from "@/components/SorobanIDE";

/**
 * Global MDX component map. Custom components (CalloutBox, ApiEndpoint,
 * CodeBlock, MultiCodeBlock, EventExplorer, ApiPlayground, CapTableVisualizer)
 * are made available to every `.mdx` page without per-file imports, and
 * internal links use the Next.js router.
 */
export function useMDXComponents(components: MDXComponents): MDXComponents {
  return {
    a: ({ href = "", children, ...props }) => {
      const isInternal = href.startsWith("/") || href.startsWith("#");
      if (isInternal) {
        return (
          <Link href={href} {...props}>
            {children}
          </Link>
        );
      }
      return (
        <a href={href} target="_blank" rel="noopener noreferrer" {...props}>
          {children}
        </a>
      );
    },
    CalloutBox,
    CodeBlock,
    MultiCodeBlock,
    EventExplorer,
    ApiEndpoint,
    ApiPlayground,
    CapTableVisualizer,
    ProspectusViewer,
    LazyProspectusViewer,
    SorobanIDE,
    ...components,
  };
}
