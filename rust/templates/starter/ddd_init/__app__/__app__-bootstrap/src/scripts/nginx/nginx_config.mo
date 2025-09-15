upstream {{PROXY_NAME}} {
    server 127.0.0.1:{{PORT}}  weight=10 max_fails=2 fail_timeout=30s;
}

server {
    listen                   80;
    # server_name              *.renyan.com *.renyan.local  *.renyan.org
    access_log               {{NGINX_LOG_PATH}}/{{DOMAIN}}/{{DOMAIN}}_access.log main;
    error_log                {{NGINX_LOG_PATH}}/{{DOMAIN}}/{{DOMAIN}}_error.log warn;
    error_page 411 = @my_error;
    location / {
        proxy_next_upstream     http_500 http_502 http_503 http_504 error timeout invalid_header;
        proxy_set_header        Host  $host;
        proxy_set_header        X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_pass              http://{{PROXY_NAME}};
        expires                 0;

        # for  nginx monitor
        log_by_lua '
             jdn = require("stat");
             jdn.log()
    ';
    }
    # for  nginx monitor
    location = /stat/service {
        access_by_lua_file lua/token.lua;
        content_by_lua '
            cjson = require("cjson")
            ocal res = {}
            res["data"] = "nginx-1.9.7"
            res["success"] = true
            ngx.say(cjson.encode(res))
        ';
    }

    # for nginx monitor
    location = /stat/status {
        access_by_lua_file lua/token.lua;
        content_by_lua '
            cjson = require("cjson")
            jdn = require("stat");
            local res = {}
            res["data"] = jdn.stat()
            res["success"] = true
            ngx.say(cjson.encode(res))
        ';
    }
}
