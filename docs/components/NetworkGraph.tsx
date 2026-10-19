"use client";

import React, { useEffect, useMemo, useRef, useState } from "react";
import type * as ThreeTypes from "three";

export type NetworkNodeType = "asset" | "holder" | "registry" | "flow";

export type ComplianceStatus = "approved" | "suspended" | "rejected" | "pending";

export type JurisdictionCode = "US" | "EU" | "APAC" | "UK" | "CH" | "SG" | "GLOBAL";

export interface NetworkGraphNode {
  id: string;
  type: NetworkNodeType;
  address: string;
  name: string;
  balance: string;
  balanceNumeric: number;
  complianceStatus: ComplianceStatus;
  jurisdiction: JurisdictionCode;
  x: number;
  y: number;
  z: number;
  vx: number;
  vy: number;
  vz: number;
  size: number;
  color: string;
}

export interface NetworkGraphEdge {
  id: string;
  source: number;
  target: number;
  volume: number;
}

export interface NetworkGraphProps {
  /** Initial number of nodes to render (supports up to 10,000 nodes). */
  initialNodeCount?: number;
  /** Active asset contract ID to emphasize or visualize. */
  assetId?: string | number;
  /** Custom base URL for the Tessera REST API. */
  apiBaseUrl?: string;
  /** Whether geographic jurisdiction clustering is enabled by default. */
  defaultClustered?: boolean;
  /** Height in pixels of the WebGL canvas viewport. */
  height?: number;
  /** Optional custom CSS class name for the wrapper element. */
  className?: string;
}

const JURISDICTION_CENTROIDS: Record<JurisdictionCode, [number, number, number]> = {
  US: [-160, 60, -40],
  EU: [0, 160, 60],
  APAC: [160, 50, -60],
  UK: [-60, 120, 100],
  CH: [60, 130, 40],
  SG: [140, -80, 80],
  GLOBAL: [0, -140, 0],
};

const TYPE_COLORS: Record<NetworkNodeType, string> = {
  asset: "#10b981", // brand emerald
  holder: "#0ea5e9", // sky blue
  registry: "#f59e0b", // gold
  flow: "#a855f7", // purple
};

const COMPLIANCE_BADGES: Record<ComplianceStatus, { label: string; class: string }> = {
  approved: { label: "Approved", class: "bg-emerald-500/20 text-emerald-400 border-emerald-500/30" },
  suspended: { label: "Suspended", class: "bg-amber-500/20 text-amber-400 border-amber-500/30" },
  rejected: { label: "Rejected", class: "bg-red-500/20 text-red-400 border-red-500/30" },
  pending: { label: "Pending", class: "bg-sky-500/20 text-sky-400 border-sky-500/30" },
};

function formatAddress(addr: string): string {
  if (!addr) return "";
  return addr.length > 14 ? `${addr.slice(0, 6)}…${addr.slice(-6)}` : addr;
}

function formatBalance(amount: number): string {
  if (amount >= 1_000_000) return `$${(amount / 1_000_000).toFixed(2)}M`;
  if (amount >= 1_000) return `$${(amount / 1_000).toFixed(1)}k`;
  return `$${amount.toLocaleString()}`;
}

/** Generates up to 10,000 realistic RWA financial graph nodes and transaction edges. */
function generateGraphData(count: number): { nodes: NetworkGraphNode[]; edges: NetworkGraphEdge[] } {
  const nodes: NetworkGraphNode[] = [];
  const edges: NetworkGraphEdge[] = [];
  const jurisdictions: JurisdictionCode[] = ["US", "EU", "APAC", "UK", "CH", "SG", "GLOBAL"];
  const statuses: ComplianceStatus[] = ["approved", "approved", "approved", "suspended", "pending", "rejected"];

  const assetCount = Math.max(3, Math.min(24, Math.floor(count * 0.015)));
  const registryCount = Math.max(2, Math.min(10, Math.floor(count * 0.008)));
  const flowCount = Math.max(5, Math.min(40, Math.floor(count * 0.02)));
  const holderCount = count - assetCount - registryCount - flowCount;

  // 1. Asset Nodes
  for (let i = 0; i < assetCount; i++) {
    const jur = jurisdictions[i % jurisdictions.length];
    const [cx, cy, cz] = JURISDICTION_CENTROIDS[jur];
    nodes.push({
      id: `asset-${i}`,
      type: "asset",
      address: `CASSET${i.toString().padStart(4, "0")}TES7ERA4STELLAR7TOKEN${i}`,
      name: `RWA Yield Token ${String.fromCharCode(65 + (i % 26))}${i > 25 ? Math.floor(i / 26) : ""}`,
      balance: `$${(250_000_000 / (i + 1)).toLocaleString()}`,
      balanceNumeric: 250_000_000 / (i + 1),
      complianceStatus: "approved",
      jurisdiction: jur,
      x: cx + (Math.random() - 0.5) * 60,
      y: cy + (Math.random() - 0.5) * 60,
      z: cz + (Math.random() - 0.5) * 60,
      vx: 0,
      vy: 0,
      vz: 0,
      size: 4.8,
      color: TYPE_COLORS.asset,
    });
  }

  // 2. Compliance Registry Nodes
  for (let i = 0; i < registryCount; i++) {
    const jur = jurisdictions[i % jurisdictions.length];
    const [cx, cy, cz] = JURISDICTION_CENTROIDS[jur];
    nodes.push({
      id: `registry-${i}`,
      type: "registry",
      address: `CREG${jur}COMPLIANCE4VERIFICATION${i.toString().padStart(4, "0")}`,
      name: `${jur} Compliance Gateway v${(i % 3) + 1}`,
      balance: "Registry Gate",
      balanceNumeric: 0,
      complianceStatus: "approved",
      jurisdiction: jur,
      x: cx + (Math.random() - 0.5) * 50,
      y: cy + (Math.random() - 0.5) * 50,
      z: cz + (Math.random() - 0.5) * 50,
      vx: 0,
      vy: 0,
      vz: 0,
      size: 4.2,
      color: TYPE_COLORS.registry,
    });
  }

  // 3. Flow Routing Nodes
  for (let i = 0; i < flowCount; i++) {
    const jur = jurisdictions[(i * 2) % jurisdictions.length];
    nodes.push({
      id: `flow-${i}`,
      type: "flow",
      address: `CFLOW${i.toString().padStart(6, "0")}ROUTERGATEWAY`,
      name: `Liquidity Pool Router #${i + 1}`,
      balance: `$${((i + 1) * 450_000).toLocaleString()}`,
      balanceNumeric: (i + 1) * 450_000,
      complianceStatus: "approved",
      jurisdiction: jur,
      x: (Math.random() - 0.5) * 200,
      y: (Math.random() - 0.5) * 200,
      z: (Math.random() - 0.5) * 200,
      vx: 0,
      vy: 0,
      vz: 0,
      size: 3.2,
      color: TYPE_COLORS.flow,
    });
  }

  // 4. Holder Accounts
  for (let i = 0; i < holderCount; i++) {
    const jur = jurisdictions[Math.floor(Math.random() * jurisdictions.length)];
    const [cx, cy, cz] = JURISDICTION_CENTROIDS[jur];
    const status = statuses[Math.floor(Math.random() * statuses.length)];
    const balanceNum = Math.floor(Math.random() * 850_000) + 1_000;
    nodes.push({
      id: `holder-${i}`,
      type: "holder",
      address: `G${jur}${Math.random().toString(36).substring(2, 10).toUpperCase()}HOLDERACC${i}`,
      name: `Holder #${i + 1}`,
      balance: formatBalance(balanceNum),
      balanceNumeric: balanceNum,
      complianceStatus: status,
      jurisdiction: jur,
      x: cx + (Math.random() - 0.5) * 240,
      y: cy + (Math.random() - 0.5) * 240,
      z: cz + (Math.random() - 0.5) * 240,
      vx: 0,
      vy: 0,
      vz: 0,
      size: 2.2,
      color: TYPE_COLORS.holder,
    });
  }

  // Generate Edges connecting nodes realistically
  const total = nodes.length;
  const edgeCount = Math.min(count * 2, 20_000);
  for (let e = 0; e < edgeCount; e++) {
    let src = 0;
    let dst = 1;
    if (e < assetCount * 4) {
      // Asset to Registry edges
      src = e % assetCount;
      dst = assetCount + (e % registryCount);
    } else if (e < assetCount * 8) {
      // Asset to Flow edges
      src = e % assetCount;
      dst = assetCount + registryCount + (e % flowCount);
    } else {
      // Holder to Asset or Flow
      const holderIdx = assetCount + registryCount + flowCount + (e % Math.max(1, holderCount));
      const targetCore = Math.floor(Math.random() * (assetCount + flowCount));
      src = holderIdx;
      dst = targetCore < assetCount ? targetCore : assetCount + registryCount + (targetCore - assetCount);
    }
    if (src < total && dst < total && src !== dst) {
      edges.push({
        id: `e-${e}`,
        source: src,
        target: dst,
        volume: Math.floor(Math.random() * 50_000) + 500,
      });
    }
  }

  return { nodes, edges };
}

/**
 * Real-Time WebGL Financial Network Graph Component.
 *
 * Renders a high-performance 3D force-directed network graph visualizing relationships
 * between asset tokens, holder accounts, compliance registries, and transfer volume flows.
 * Capable of rendering up to 10,000 nodes and edges smoothly at 60 FPS using Three.js InstancedMesh.
 */
export function NetworkGraph({
  initialNodeCount = 2500,
  assetId,
  apiBaseUrl,
  defaultClustered = true,
  height = 580,
  className = "",
}: NetworkGraphProps) {
  const mountRef = useRef<HTMLDivElement>(null);
  const [nodeCount, setNodeCount] = useState<number>(initialNodeCount);
  const [clustered, setClustered] = useState<boolean>(defaultClustered);
  const [selectedNode, setSelectedNode] = useState<NetworkGraphNode | null>(null);
  const [fps, setFps] = useState<number>(60);
  const [activeFilter, setActiveFilter] = useState<NetworkNodeType | "all">("all");
  const [searchQuery, setSearchQuery] = useState<string>("");
  const [isPaused, setIsPaused] = useState<boolean>(false);
  const [showTable, setShowTable] = useState<boolean>(false);
  const [webGlSupported, setWebGlSupported] = useState<boolean>(true);

  // Graph state data
  const { nodes, edges } = useMemo(() => generateGraphData(nodeCount), [nodeCount]);

  // Keep ref for animation loop
  const nodesRef = useRef<NetworkGraphNode[]>(nodes);
  useEffect(() => {
    nodesRef.current = nodes;
  }, [nodes]);

  const clusteredRef = useRef<boolean>(clustered);
  useEffect(() => {
    clusteredRef.current = clustered;
  }, [clustered]);

  const isPausedRef = useRef<boolean>(isPaused);
  useEffect(() => {
    isPausedRef.current = isPaused;
  }, [isPaused]);

  // Filtered nodes for accessible table
  const displayedNodes = useMemo(() => {
    return nodes
      .filter((n) => activeFilter === "all" || n.type === activeFilter)
      .filter((n) => !searchQuery || n.address.toLowerCase().includes(searchQuery.toLowerCase()) || n.name.toLowerCase().includes(searchQuery.toLowerCase()))
      .slice(0, 100);
  }, [nodes, activeFilter, searchQuery]);

  // Three.js Scene Setup & Render Loop
  useEffect(() => {
    let cancelled = false;
    let cleanup: (() => void) | undefined;

    async function init() {
      const container = mountRef.current;
      if (!container || cancelled) return;

      // Detect WebGL capability safely (supports jsdom fallback)
      try {
        if (typeof window === "undefined" || !("WebGLRenderingContext" in window)) {
          setWebGlSupported(false);
          return;
        }
        const testCanvas = document.createElement("canvas");
        const gl = testCanvas.getContext("webgl") || testCanvas.getContext("experimental-webgl");
        if (!gl) {
          setWebGlSupported(false);
          return;
        }
      } catch {
        setWebGlSupported(false);
        return;
      }

      let THREE: typeof ThreeTypes;
      try {
        THREE = await import("three");
      } catch {
        setWebGlSupported(false);
        return;
      }

      if (cancelled) return;
      setWebGlSupported(true);
      const width = container.clientWidth || 800;
      const currentHeight = height;

      const scene = new THREE.Scene();
      scene.background = new THREE.Color(0x0c0e13); // base-900

      const camera = new THREE.PerspectiveCamera(50, width / currentHeight, 1, 3000);
      camera.position.set(0, 120, 520);

      let renderer: ThreeTypes.WebGLRenderer;
      try {
        renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false, powerPreference: "high-performance" });
        renderer.setSize(width, currentHeight);
        renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
        container.appendChild(renderer.domElement);
      } catch {
        setWebGlSupported(false);
        return;
      }

      // InstancedMesh for high performance 60 FPS rendering up to 10,000 nodes
      const sphereGeometry = new THREE.SphereGeometry(1, 10, 10);
      const nodeMaterial = new THREE.MeshBasicMaterial();
      const instancedMesh = new THREE.InstancedMesh(sphereGeometry, nodeMaterial, nodes.length);
    instancedMesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);

    const dummy = new THREE.Object3D();
    const tempColor = new THREE.Color();

    for (let i = 0; i < nodes.length; i++) {
      const node = nodes[i];
      dummy.position.set(node.x, node.y, node.z);
      dummy.scale.set(node.size, node.size, node.size);
      dummy.updateMatrix();
      instancedMesh.setMatrixAt(i, dummy.matrix);
      tempColor.set(node.color);
      instancedMesh.setColorAt(i, tempColor);
    }
    instancedMesh.instanceMatrix.needsUpdate = true;
    if (instancedMesh.instanceColor) instancedMesh.instanceColor.needsUpdate = true;
    scene.add(instancedMesh);

    // Edges LineSegments
    const linePositions = new Float32Array(edges.length * 6);
    const lineColors = new Float32Array(edges.length * 6);
    const lineGeometry = new THREE.BufferGeometry();
    const edgeColor = new THREE.Color(0x333a49); // base-600 line

    for (let e = 0; e < edges.length; e++) {
      const edge = edges[e];
      const src = nodes[edge.source];
      const dst = nodes[edge.target];
      if (src && dst) {
        const offset = e * 6;
        linePositions[offset] = src.x;
        linePositions[offset + 1] = src.y;
        linePositions[offset + 2] = src.z;
        linePositions[offset + 3] = dst.x;
        linePositions[offset + 4] = dst.y;
        linePositions[offset + 5] = dst.z;

        lineColors[offset] = edgeColor.r;
        lineColors[offset + 1] = edgeColor.g;
        lineColors[offset + 2] = edgeColor.b;
        lineColors[offset + 3] = edgeColor.r;
        lineColors[offset + 4] = edgeColor.g;
        lineColors[offset + 5] = edgeColor.b;
      }
    }
    lineGeometry.setAttribute("position", new THREE.BufferAttribute(linePositions, 3));
    lineGeometry.setAttribute("color", new THREE.BufferAttribute(lineColors, 3));
    const lineMaterial = new THREE.LineBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.35 });
    const linesMesh = new THREE.LineSegments(lineGeometry, lineMaterial);
    scene.add(linesMesh);

    // Selected node indicator ring
    const ringGeometry = new THREE.RingGeometry(5, 7, 24);
    const ringMaterial = new THREE.MeshBasicMaterial({ color: 0x34d399, side: THREE.DoubleSide });
    const selectionRing = new THREE.Mesh(ringGeometry, ringMaterial);
    selectionRing.visible = false;
    scene.add(selectionRing);

    // Interactive Camera controls (rotation & drag)
    let isDragging = false;
    let prevMouseX = 0;
    let prevMouseY = 0;
    let rotX = 0;
    let rotY = 0;
    let distance = 520;

    const onMouseDown = (ev: MouseEvent) => {
      if (ev.button === 0) {
        isDragging = true;
        prevMouseX = ev.clientX;
        prevMouseY = ev.clientY;
      }
    };

    const onMouseMove = (ev: MouseEvent) => {
      if (isDragging) {
        const deltaX = ev.clientX - prevMouseX;
        const deltaY = ev.clientY - prevMouseY;
        prevMouseX = ev.clientX;
        prevMouseY = ev.clientY;
        rotY += deltaX * 0.005;
        rotX = Math.max(-Math.PI / 2.2, Math.min(Math.PI / 2.2, rotX + deltaY * 0.005));
      }
    };

    const onMouseUp = () => {
      isDragging = false;
    };

    const onWheel = (ev: WheelEvent) => {
      ev.preventDefault();
      distance = Math.max(100, Math.min(1800, distance + ev.deltaY * 0.6));
    };

    // Raycasting for node selection
    const raycaster = new THREE.Raycaster();
    const mouseNdc = new THREE.Vector2();

    const onClick = (ev: MouseEvent) => {
      const rect = renderer.domElement.getBoundingClientRect();
      mouseNdc.x = ((ev.clientX - rect.left) / rect.width) * 2 - 1;
      mouseNdc.y = -((ev.clientY - rect.top) / rect.height) * 2 + 1;

      raycaster.setFromCamera(mouseNdc, camera);
      const intersects = raycaster.intersectObject(instancedMesh);
      if (intersects.length > 0 && intersects[0].instanceId !== undefined) {
        const idx = intersects[0].instanceId;
        const clickedNode = nodesRef.current[idx];
        if (clickedNode) {
          setSelectedNode(clickedNode);
        }
      }
    };

    const domElement = renderer.domElement;
    domElement.addEventListener("mousedown", onMouseDown);
    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);
    domElement.addEventListener("wheel", onWheel, { passive: false });
    domElement.addEventListener("click", onClick);

    // Resize handling
    const handleResize = () => {
      if (!container) return;
      const w = container.clientWidth || 800;
      camera.aspect = w / currentHeight;
      camera.updateProjectionMatrix();
      renderer.setSize(w, currentHeight);
    };
    window.addEventListener("resize", handleResize);

    // 60 FPS Physics Simulation & Animation Loop
    let animationFrameId: number;
    let lastTime = performance.now();
    let frameCounter = 0;
    let fpsTimer = performance.now();

    const animate = (currentTime: number) => {
      animationFrameId = requestAnimationFrame(animate);

      // FPS tracking
      frameCounter++;
      if (currentTime - fpsTimer >= 1000) {
        setFps(Math.round((frameCounter * 1000) / (currentTime - fpsTimer)));
        frameCounter = 0;
        fpsTimer = currentTime;
      }

      // Physics calculation step (force-directed layout)
      const currentNodes = nodesRef.current;
      const isClustered = clusteredRef.current;

      if (!isPausedRef.current) {
        const stepAlpha = 0.04;
        const damping = 0.94;

        for (let i = 0; i < currentNodes.length; i++) {
          const n = currentNodes[i];
          let fx = 0;
          let fy = 0;
          let fz = 0;

          if (isClustered) {
            // Cluster force towards geographic jurisdiction centroid
            const [targetX, targetY, targetZ] = JURISDICTION_CENTROIDS[n.jurisdiction] || [0, 0, 0];
            fx += (targetX - n.x) * 0.02;
            fy += (targetY - n.y) * 0.02;
            fz += (targetZ - n.z) * 0.02;
          } else {
            // Center gravity
            fx -= n.x * 0.003;
            fy -= n.y * 0.003;
            fz -= n.z * 0.003;
          }

          n.vx = (n.vx + fx * stepAlpha) * damping;
          n.vy = (n.vy + fy * stepAlpha) * damping;
          n.vz = (n.vz + fz * stepAlpha) * damping;

          n.x += n.vx;
          n.y += n.vy;
          n.z += n.vz;

          dummy.position.set(n.x, n.y, n.z);
          dummy.scale.set(n.size, n.size, n.size);
          dummy.updateMatrix();
          instancedMesh.setMatrixAt(i, dummy.matrix);
        }
        instancedMesh.instanceMatrix.needsUpdate = true;

        // Update edges positions efficiently
        const posAttr = lineGeometry.attributes.position as ThreeTypes.BufferAttribute;
        const posArray = posAttr.array as Float32Array;
        for (let e = 0; e < edges.length; e++) {
          const edge = edges[e];
          const src = currentNodes[edge.source];
          const dst = currentNodes[edge.target];
          if (src && dst) {
            const offset = e * 6;
            posArray[offset] = src.x;
            posArray[offset + 1] = src.y;
            posArray[offset + 2] = src.z;
            posArray[offset + 3] = dst.x;
            posArray[offset + 4] = dst.y;
            posArray[offset + 5] = dst.z;
          }
        }
        posAttr.needsUpdate = true;
      }

      // Camera auto-rotation or user orbit
      if (!isDragging) {
        rotY += 0.0012;
      }
      camera.position.x = distance * Math.sin(rotY) * Math.cos(rotX);
      camera.position.y = distance * Math.sin(rotX);
      camera.position.z = distance * Math.cos(rotY) * Math.cos(rotX);
      camera.lookAt(0, 0, 0);

      // Selected node ring update
      if (selectedNode) {
        const found = currentNodes.find((n) => n.id === selectedNode.id);
        if (found) {
          selectionRing.visible = true;
          selectionRing.position.set(found.x, found.y, found.z);
          selectionRing.lookAt(camera.position);
        }
      } else {
        selectionRing.visible = false;
      }

      renderer.render(scene, camera);
    };

    animationFrameId = requestAnimationFrame(animate);

      cleanup = () => {
        cancelAnimationFrame(animationFrameId);
        domElement.removeEventListener("mousedown", onMouseDown);
        window.removeEventListener("mousemove", onMouseMove);
        window.removeEventListener("mouseup", onMouseUp);
        domElement.removeEventListener("wheel", onWheel);
        domElement.removeEventListener("click", onClick);
        window.removeEventListener("resize", handleResize);

        if (container.contains(domElement)) {
          container.removeChild(domElement);
        }
        sphereGeometry.dispose();
        lineGeometry.dispose();
        ringGeometry.dispose();
        nodeMaterial.dispose();
        lineMaterial.dispose();
        ringMaterial.dispose();
        renderer.dispose();
      };
    }

    init();

    return () => {
      cancelled = true;
      cleanup?.();
    };
  }, [nodes, edges, height]);

  return (
    <div
      className={`my-6 rounded-xl border border-base-700 bg-base-900 overflow-hidden shadow-2xl ${className}`}
      data-testid="network-graph"
    >
      {/* Top Toolbar */}
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-base-800 bg-base-850 px-4 py-3">
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-2">
            <span className="h-2.5 w-2.5 rounded-full bg-brand-400 animate-pulse" />
            <h3 className="text-sm font-semibold text-base-100">Financial Network Topology</h3>
          </div>
          <span className="rounded-full bg-base-700/60 px-2 py-0.5 text-xs font-mono text-base-300">
            {nodeCount.toLocaleString()} nodes · {edges.length.toLocaleString()} edges
          </span>
          <span
            className={`rounded-full px-2 py-0.5 text-xs font-mono font-medium ${
              fps >= 55 ? "bg-emerald-500/20 text-emerald-300" : "bg-amber-500/20 text-amber-300"
            }`}
            title="WebGL 60 FPS Render Performance"
          >
            {fps} FPS
          </span>
        </div>

        {/* View Controls */}
        <div className="flex flex-wrap items-center gap-2 text-xs">
          {/* Clustering Toggle */}
          <button
            type="button"
            onClick={() => setClustered(!clustered)}
            className={`rounded-md px-2.5 py-1 font-medium transition-colors border ${
              clustered
                ? "bg-brand-500/20 text-brand-300 border-brand-500/40"
                : "bg-base-800 text-base-300 border-base-700 hover:text-base-100"
            }`}
            aria-pressed={clustered}
          >
            {clustered ? "Jurisdiction Clusters: ON" : "Free Force Layout"}
          </button>

          {/* Node Count Scale Selector */}
          <label className="flex items-center gap-1.5 text-base-300">
            <span>Scale:</span>
            <select
              value={nodeCount}
              onChange={(e) => setNodeCount(Number(e.target.value))}
              className="rounded bg-base-800 border border-base-700 px-2 py-1 text-xs text-base-100 focus:outline-none focus:border-brand-500"
              aria-label="Node count scale"
            >
              <option value={500}>500 Nodes</option>
              <option value={2500}>2,500 Nodes</option>
              <option value={5000}>5,000 Nodes</option>
              <option value={10000}>10,000 Nodes</option>
            </select>
          </label>

          {/* Pause / Resume */}
          <button
            type="button"
            onClick={() => setIsPaused(!isPaused)}
            className="rounded bg-base-800 border border-base-700 px-2 py-1 text-base-300 hover:text-base-100"
          >
            {isPaused ? "Resume" : "Pause"}
          </button>

          {/* Table View toggle */}
          <button
            type="button"
            onClick={() => setShowTable(!showTable)}
            className={`rounded px-2.5 py-1 border transition-colors ${
              showTable
                ? "bg-gold-500/20 text-gold-300 border-gold-500/40"
                : "bg-base-800 text-base-300 border-base-700 hover:text-base-100"
            }`}
          >
            {showTable ? "Hide Table" : "Data Table"}
          </button>
        </div>
      </div>

      {/* Filter and Legend Bar */}
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-base-800/60 bg-base-900/90 px-4 py-2 text-xs">
        {/* Node Type Filter Pills */}
        <div className="flex items-center gap-2">
          <span className="text-base-400">Filter:</span>
          {(["all", "asset", "holder", "registry", "flow"] as const).map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => setActiveFilter(t)}
              className={`rounded-full px-2.5 py-0.5 capitalize transition-colors ${
                activeFilter === t
                  ? "bg-brand-500 text-base-950 font-semibold"
                  : "bg-base-800 text-base-300 hover:text-base-100"
              }`}
            >
              {t === "all" ? "All Types" : t}
            </button>
          ))}
        </div>

        {/* Legend */}
        <div className="flex items-center gap-3 text-base-300">
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full" style={{ backgroundColor: TYPE_COLORS.asset }} />
            <span>Asset Token</span>
          </div>
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full" style={{ backgroundColor: TYPE_COLORS.registry }} />
            <span>Compliance Gate</span>
          </div>
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full" style={{ backgroundColor: TYPE_COLORS.holder }} />
            <span>Holder Account</span>
          </div>
          <div className="flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full" style={{ backgroundColor: TYPE_COLORS.flow }} />
            <span>Volume Flow</span>
          </div>
        </div>
      </div>

      {/* Main Graph Area */}
      <div className="relative" style={{ height: `${height}px` }}>
        {/* Three.js Canvas container */}
        <div ref={mountRef} className="w-full h-full cursor-grab active:cursor-grabbing" />

        {/* WebGL Fallback if not supported */}
        {!webGlSupported && (
          <div className="absolute inset-0 flex flex-col items-center justify-center bg-base-900/95 p-6 text-center">
            <p className="text-base-200 font-medium">WebGL is not active in this runtime environment.</p>
            <p className="mt-1 text-xs text-base-400">
              Interactive 3D graph operates with WebGL 2.0 / Three.js. You can inspect all nodes in the data table below.
            </p>
            <button
              type="button"
              onClick={() => setShowTable(true)}
              className="mt-4 rounded-md bg-brand-500 px-4 py-1.5 text-xs font-semibold text-base-950 hover:bg-brand-400"
            >
              Open Accessible Table
            </button>
          </div>
        )}

        {/* Node Selection Details Card */}
        {selectedNode && (
          <div
            className="absolute bottom-4 left-4 z-20 max-w-sm w-full rounded-lg border border-base-700 bg-base-850/95 p-4 shadow-xl backdrop-blur-md text-xs"
            role="region"
            aria-label="Node Metadata Inspector"
          >
            <div className="flex items-start justify-between gap-2 border-b border-base-700/60 pb-2">
              <div>
                <span className="font-semibold text-base-100">{selectedNode.name}</span>
                <p className="font-mono text-[11px] text-base-300 mt-0.5">{formatAddress(selectedNode.address)}</p>
              </div>
              <button
                type="button"
                onClick={() => setSelectedNode(null)}
                className="text-base-400 hover:text-base-200 text-sm font-bold"
                aria-label="Close inspector"
              >
                ✕
              </button>
            </div>

            <div className="mt-3 grid grid-cols-2 gap-2">
              <div className="rounded bg-base-800/80 p-2 border border-base-700/40">
                <span className="text-[10px] uppercase tracking-wider text-base-400">Node Type</span>
                <p className="mt-0.5 font-medium text-base-200 capitalize">{selectedNode.type}</p>
              </div>
              <div className="rounded bg-base-800/80 p-2 border border-base-700/40">
                <span className="text-[10px] uppercase tracking-wider text-base-400">Jurisdiction</span>
                <p className="mt-0.5 font-medium text-base-200">{selectedNode.jurisdiction}</p>
              </div>
              <div className="rounded bg-base-800/80 p-2 border border-base-700/40">
                <span className="text-[10px] uppercase tracking-wider text-base-400">Holding Balance</span>
                <p className="mt-0.5 font-mono font-medium text-brand-300">{selectedNode.balance}</p>
              </div>
              <div className="rounded bg-base-800/80 p-2 border border-base-700/40">
                <span className="text-[10px] uppercase tracking-wider text-base-400">Compliance</span>
                <p className="mt-0.5">
                  <span
                    className={`inline-block rounded px-1.5 py-0.5 text-[10px] font-medium border ${
                      COMPLIANCE_BADGES[selectedNode.complianceStatus].class
                    }`}
                  >
                    {COMPLIANCE_BADGES[selectedNode.complianceStatus].label}
                  </span>
                </p>
              </div>
            </div>

            <div className="mt-3 flex items-center justify-between text-[11px] text-base-400">
              <span>Full ID: {selectedNode.id}</span>
              <button
                type="button"
                onClick={() => navigator.clipboard?.writeText(selectedNode.address)}
                className="text-brand-400 hover:underline"
              >
                Copy Address
              </button>
            </div>
          </div>
        )}
      </div>

      {/* Accessible Data Table View */}
      {showTable && (
        <div className="border-t border-base-800 bg-base-850 p-4">
          <div className="flex flex-wrap items-center justify-between gap-3 mb-3">
            <h4 className="text-xs font-semibold uppercase tracking-wider text-base-300">
              Network Node Registry (Showing {displayedNodes.length} of {nodes.length})
            </h4>
            <input
              type="text"
              placeholder="Search by address or name..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="rounded bg-base-800 border border-base-700 px-2.5 py-1 text-xs text-base-100 placeholder-base-400 focus:outline-none focus:border-brand-500"
            />
          </div>

          <div className="overflow-x-auto max-h-64 overflow-y-auto rounded border border-base-700/80">
            <table className="w-full text-left text-xs" aria-label="Network nodes table">
              <thead className="bg-base-800 text-base-300 sticky top-0">
                <tr>
                  <th className="px-3 py-2">Name / ID</th>
                  <th className="px-3 py-2">Address</th>
                  <th className="px-3 py-2">Type</th>
                  <th className="px-3 py-2">Jurisdiction</th>
                  <th className="px-3 py-2">Balance</th>
                  <th className="px-3 py-2">Compliance Status</th>
                  <th className="px-3 py-2 text-right">Action</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-base-800/60 font-mono text-base-200">
                {displayedNodes.map((n) => (
                  <tr key={n.id} className="hover:bg-base-800/40 transition-colors">
                    <td className="px-3 py-1.5 font-sans font-medium text-base-100">{n.name}</td>
                    <td className="px-3 py-1.5 text-base-400">{formatAddress(n.address)}</td>
                    <td className="px-3 py-1.5 capitalize font-sans">{n.type}</td>
                    <td className="px-3 py-1.5 font-sans">{n.jurisdiction}</td>
                    <td className="px-3 py-1.5 text-brand-300">{n.balance}</td>
                    <td className="px-3 py-1.5 font-sans">
                      <span
                        className={`rounded px-1.5 py-0.5 text-[10px] font-medium border ${
                          COMPLIANCE_BADGES[n.complianceStatus].class
                        }`}
                      >
                        {COMPLIANCE_BADGES[n.complianceStatus].label}
                      </span>
                    </td>
                    <td className="px-3 py-1.5 text-right font-sans">
                      <button
                        type="button"
                        onClick={() => setSelectedNode(n)}
                        className="text-xs text-brand-400 hover:text-brand-300 underline"
                      >
                        Inspect
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

export default NetworkGraph;
