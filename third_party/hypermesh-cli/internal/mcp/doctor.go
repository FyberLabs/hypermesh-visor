package mcp

import (
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"time"
)

// Check is one doctor result for a server.
type Check struct {
	Name    string `json:"name"`
	OK      bool   `json:"ok"`
	Transport string `json:"transport"`
	Detail  string `json:"detail"`
}

// Doctor probes servers in a profile without calling tools.
// Stdio: command must resolve on PATH (or be an absolute path).
// HTTP/SSE: URL must parse and the host must accept a TCP connect.
func (s Store) Doctor(profileName string) (string, []Check, error) {
	name, servers, err := s.ActiveServers(profileName)
	if err != nil {
		return "", nil, err
	}
	checks := make([]Check, 0, len(servers))
	for _, id := range sortedKeys(servers) {
		checks = append(checks, checkServer(id, servers[id]))
	}
	return name, checks, nil
}

func checkServer(name string, server Server) Check {
	c := Check{Name: name, Transport: server.Transport()}
	switch server.Transport() {
	case "stdio":
		path, err := exec.LookPath(server.Command)
		if err != nil {
			if filepathIsAbs(server.Command) {
				if _, statErr := os.Stat(server.Command); statErr != nil {
					c.Detail = fmt.Sprintf("command not found: %s", server.Command)
					return c
				}
				path = server.Command
			} else {
				c.Detail = fmt.Sprintf("command not on PATH: %s", server.Command)
				return c
			}
		}
		c.OK = true
		c.Detail = fmt.Sprintf("command %s", path)
	case "http", "sse":
		u, err := url.Parse(server.URL)
		if err != nil || u.Host == "" {
			c.Detail = fmt.Sprintf("invalid url: %s", server.URL)
			return c
		}
		host := u.Host
		if !strings.Contains(host, ":") {
			if u.Scheme == "https" {
				host += ":443"
			} else {
				host += ":80"
			}
		}
		conn, err := net.DialTimeout("tcp", host, 2*time.Second)
		if err != nil {
			// Still OK for doctor if URL is well-formed but host is down —
			// report failure so the user can fix config.
			c.Detail = fmt.Sprintf("cannot reach %s: %v", host, err)
			return c
		}
		_ = conn.Close()
		c.OK = true
		c.Detail = fmt.Sprintf("reachable %s", host)
		// Optional HEAD/GET without failing closed on 404/405.
		client := &http.Client{Timeout: 2 * time.Second}
		req, err := http.NewRequest(http.MethodGet, server.URL, nil)
		if err == nil {
			resp, err := client.Do(req)
			if err == nil {
				_ = resp.Body.Close()
				c.Detail = fmt.Sprintf("reachable %s (HTTP %d)", host, resp.StatusCode)
			}
		}
	default:
		c.Detail = fmt.Sprintf("unknown transport %q", server.Transport())
	}
	return c
}

func filepathIsAbs(p string) bool {
	return strings.HasPrefix(p, "/") || (len(p) > 2 && p[1] == ':')
}
