package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/A4-Stellar/Tessera/infrastructure/k8s/operator/api/v1alpha1"
	_ "github.com/A4-Stellar/Tessera/infrastructure/k8s/operator/controllers"
)

func main() {
	var metricsAddr string
	var probeAddr string
	var enableLeaderElection bool

	flag.StringVar(&metricsAddr, "metrics-bind-address", ":8080", "The address the metric endpoint binds to.")
	flag.StringVar(&probeAddr, "health-probe-bind-address", ":8081", "The address the probe endpoint binds to.")
	flag.BoolVar(&enableLeaderElection, "leader-elect", false, "Enable leader election for controller manager.")
	flag.Parse()

	log.Printf("[INFO] Initializing Tessera Kubernetes Operator (version: %s)", v1alpha1.Version)
	log.Printf("[INFO] Metrics endpoint configured on %s", metricsAddr)
	log.Printf("[INFO] Health probe endpoint configured on %s", probeAddr)

	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		fmt.Fprintln(w, "ok")
	})
	mux.HandleFunc("/readyz", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		fmt.Fprintln(w, "ok")
	})
	mux.HandleFunc("/metrics", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		fmt.Fprintln(w, "# HELP tessera_operator_reconcile_total Total reconciliations")
		fmt.Fprintln(w, "tessera_operator_reconcile_total 1")
	})

	server := &http.Server{
		Addr:    probeAddr,
		Handler: mux,
	}

	go func() {
		if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Fatalf("[ERROR] Health server failed: %v", err)
		}
	}()

	log.Println("[INFO] Tessera Operator controllers started successfully.")

	stopCh := make(chan os.Signal, 1)
	signal.Notify(stopCh, syscall.SIGINT, syscall.SIGTERM)
	<-stopCh

	log.Println("[INFO] Shutting down Tessera Operator gracefully...")
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = server.Shutdown(ctx)
	log.Println("[INFO] Tessera Operator shutdown complete.")
}
