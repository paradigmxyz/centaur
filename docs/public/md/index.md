---
title: Centaur
description: Centaur is a production control plane for shared AI agents that run in isolated sandboxes and call approved tools.
layout: landing
showOutline: false
showSidebar: false
content:
  horizontalPadding: 0px
  verticalPadding: 0px
  width: "100%"
---

import ThreadPanel from '../components/ThreadPanel'

<main className="centaur-home">
  <section className="home-hero" aria-labelledby="home-title">
    <div className="home-copy">
      <div className="home-title-lockup">
        <picture>
          <source srcSet="/brand/mark-white.png" media="(prefers-color-scheme: dark)" />
          <img className="home-mark" src="/brand/mark-black.png" alt="Centaur" />
        </picture>
        <h1 id="home-title">Run shared agents on infrastructure you control.</h1>
      </div>
      <div className="home-lede">{'Centaur stores every turn, assigns isolated sandboxes, exposes approved tools, runs durable workflows, and injects credentials at the network boundary.'}</div>

      <div className="home-actions" aria-label="Primary documentation links">
        <a className="home-button home-button-primary" href="/deploying-in-production">Deploy in production</a>
        <a className="home-button" href="/architecture">Architecture</a>
      </div>

      <div className="home-built-with" aria-label="Built by">
        <span>Built by</span>
        <div className="home-logo-row" aria-label="Paradigm and Tempo">
          <a className="home-brand" href="https://paradigm.xyz" aria-label="Paradigm">
            <img src="/paradigm-logo.svg" alt="Paradigm" />
          </a>
          <a className="home-brand home-brand-tempo" href="https://tempo.xyz" aria-label="Tempo">
            <img src="/tempo-logo.svg" alt="Tempo" />
          </a>
        </div>
      </div>
    </div>

    <div className="home-thread-demo" aria-label="Centaur thread preview">
      <ThreadPanel />
    </div>
  </section>
</main>
