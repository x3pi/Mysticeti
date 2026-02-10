// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! RPC Circuit Breaker — protects Go Master RPC calls from cascading failures.
//!
//! ## States
//! ```text
//! Closed ──(N failures)──► Open ──(cooldown)──► HalfOpen
//!   ▲                                              │
//!   └────────(probe success)───────────────────────┘
//!                                                  │
//!   Open ◄───(probe failure)───────────────────────┘
//! ```
//!
//! - **Closed**: Normal operation. All calls pass through.
//! - **Open**: Calls rejected immediately with `CircuitOpen` error.
//! - **HalfOpen**: One probe call allowed. Success → Closed, failure → Open.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Circuit breaker state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — all calls pass through
    Closed,
    /// Failures exceeded threshold — calls rejected immediately
    #[allow(dead_code)]
    Open,
    /// Cooldown expired — one probe call allowed
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "Closed"),
            CircuitState::Open => write!(f, "Open"),
            CircuitState::HalfOpen => write!(f, "HalfOpen"),
        }
    }
}

/// Configuration for the circuit breaker
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening the circuit
    #[allow(dead_code)]
    pub failure_threshold: u32,
    /// Duration to wait before transitioning from Open → HalfOpen
    pub cooldown_duration: Duration,
    /// Number of consecutive successes in HalfOpen to close the circuit
    #[allow(dead_code)]
    pub success_threshold: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            cooldown_duration: Duration::from_secs(30),
            success_threshold: 1,
        }
    }
}

/// Per-method circuit state
struct MethodCircuit {
    state: CircuitState,
    consecutive_failures: u32,
    consecutive_successes: u32,
    #[allow(dead_code)]
    last_failure_time: Option<Instant>,
    opened_at: Option<Instant>,
    total_rejections: u64,
}

impl MethodCircuit {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            consecutive_successes: 0,
            last_failure_time: None,
            opened_at: None,
            total_rejections: 0,
        }
    }
}

/// Circuit breaker for RPC methods.
///
/// Each RPC method has its own independent circuit. A failing `get_current_epoch`
/// won't block `get_last_block_number` from working.
pub struct RpcCircuitBreaker {
    circuits: std::sync::Mutex<HashMap<String, MethodCircuit>>,
    config: CircuitBreakerConfig,
}

impl RpcCircuitBreaker {
    /// Create a new circuit breaker with default configuration
    pub fn new() -> Self {
        Self {
            circuits: std::sync::Mutex::new(HashMap::new()),
            config: CircuitBreakerConfig::default(),
        }
    }

    /// Create with custom configuration
    #[allow(dead_code)]
    pub fn with_config(config: CircuitBreakerConfig) -> Self {
        Self {
            circuits: std::sync::Mutex::new(HashMap::new()),
            config,
        }
    }

    /// Check if a call to the given method is allowed.
    ///
    /// Returns `Ok(())` if the call should proceed, `Err(reason)` if rejected.
    pub fn check(&self, method: &str) -> Result<(), String> {
        let mut circuits = self.circuits.lock().unwrap();
        let circuit = circuits
            .entry(method.to_string())
            .or_insert_with(MethodCircuit::new);

        match circuit.state {
            CircuitState::Closed => Ok(()),
            CircuitState::Open => {
                // Check if cooldown has expired
                if let Some(opened_at) = circuit.opened_at {
                    if opened_at.elapsed() >= self.config.cooldown_duration {
                        // Transition to HalfOpen
                        circuit.state = CircuitState::HalfOpen;
                        circuit.consecutive_successes = 0;
                        debug!(
                            "🔌 [CIRCUIT] {} transitioning Open → HalfOpen (cooldown expired)",
                            method
                        );
                        Ok(())
                    } else {
                        circuit.total_rejections += 1;
                        let remaining = self.config.cooldown_duration - opened_at.elapsed();
                        Err(format!(
                            "Circuit open for '{}': {} consecutive failures, {}s until probe",
                            method,
                            circuit.consecutive_failures,
                            remaining.as_secs()
                        ))
                    }
                } else {
                    // Should not happen, but allow the call
                    Ok(())
                }
            }
            CircuitState::HalfOpen => {
                // Allow probe call
                Ok(())
            }
        }
    }

    /// Record a successful call
    #[allow(dead_code)]
    pub fn record_success(&self, method: &str) {
        let mut circuits = self.circuits.lock().unwrap();
        let circuit = circuits
            .entry(method.to_string())
            .or_insert_with(MethodCircuit::new);

        circuit.consecutive_failures = 0;
        circuit.consecutive_successes += 1;

        match circuit.state {
            CircuitState::HalfOpen => {
                if circuit.consecutive_successes >= self.config.success_threshold {
                    circuit.state = CircuitState::Closed;
                    circuit.opened_at = None;
                    info!(
                        "✅ [CIRCUIT] {} transitioning HalfOpen → Closed (probe succeeded)",
                        method
                    );
                }
            }
            CircuitState::Open => {
                // Shouldn't normally happen, but reset
                circuit.state = CircuitState::Closed;
                circuit.opened_at = None;
            }
            CircuitState::Closed => {
                // Normal operation
            }
        }
    }

    /// Record a failed call
    #[allow(dead_code)]
    pub fn record_failure(&self, method: &str) {
        let mut circuits = self.circuits.lock().unwrap();
        let circuit = circuits
            .entry(method.to_string())
            .or_insert_with(MethodCircuit::new);

        circuit.consecutive_failures += 1;
        circuit.consecutive_successes = 0;
        circuit.last_failure_time = Some(Instant::now());

        match circuit.state {
            CircuitState::Closed => {
                if circuit.consecutive_failures >= self.config.failure_threshold {
                    circuit.state = CircuitState::Open;
                    circuit.opened_at = Some(Instant::now());
                    warn!(
                        "🔴 [CIRCUIT] {} transitioning Closed → Open ({} consecutive failures)",
                        method, circuit.consecutive_failures
                    );
                }
            }
            CircuitState::HalfOpen => {
                // Probe failed — go back to Open
                circuit.state = CircuitState::Open;
                circuit.opened_at = Some(Instant::now());
                warn!(
                    "🔴 [CIRCUIT] {} transitioning HalfOpen → Open (probe failed)",
                    method
                );
            }
            CircuitState::Open => {
                // Already open, just count
            }
        }
    }

    /// Get the current state of a method's circuit
    #[allow(dead_code)]
    pub fn state(&self, method: &str) -> CircuitState {
        let circuits = self.circuits.lock().unwrap();
        circuits
            .get(method)
            .map(|c| c.state)
            .unwrap_or(CircuitState::Closed)
    }

    /// Get total rejections for a method
    #[allow(dead_code)]
    pub fn total_rejections(&self, method: &str) -> u64 {
        let circuits = self.circuits.lock().unwrap();
        circuits
            .get(method)
            .map(|c| c.total_rejections)
            .unwrap_or(0)
    }

    /// Get failure count for a method
    #[allow(dead_code)]
    pub fn failure_count(&self, method: &str) -> u32 {
        let circuits = self.circuits.lock().unwrap();
        circuits
            .get(method)
            .map(|c| c.consecutive_failures)
            .unwrap_or(0)
    }
}
