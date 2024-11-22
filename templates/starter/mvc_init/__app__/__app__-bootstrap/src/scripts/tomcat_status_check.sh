#!/bin/bash

CATALINA_LOG_FILE=$1  # tomcat日志路径
init_start_nginx=on   # 是否启动nginx
last_tomcat_log_end=0 # 本次tomcat启动之前日志的字节数

# 此次tomcat启动日志所占行数
tomcat_startup_line=
# tomcat状态错误默认关键词
tomcat_check_keys="Error listenerStart|Error filterStart|Fatal Error|One or more listeners failed to start|One or more components marked the context as not correctly configured|Application startup failed"
# tomcat启动成功关键词
tomcat_success_key="Tomcat started on port"
# 默认启动nginx
if [ ! -n "$init_start_nginx" ]; then
  init_start_nginx="on"
fi

if [ -f "${CATALINA_LOG_FILE}" ]; then
  last_tomcat_log_end=$(ls -l ${CATALINA_LOG_FILE} | awk '{print $5;exit}')
fi

echo "log file path:$CATALINA_LOG_FILE"
echo "init start nginx:$init_start_nginx"
echo "last tomcat log end:$last_tomcat_log_end"

function start_nginx() {
  sudo /export/servers/nginx/sbin/nginx
  sudo /export/servers/nginx/sbin/nginx -s reload

  echo -e "\nNginx starting ..."
}

if [ ! -n "$CATALINA_LOG_FILE" -a "$init_start_nginx" == "on" ]; then
  start_nginx
  exit 0
fi

function check_nginx {
  local -i retry=0
  local msg

  while true; do

    nginx_num=$(ps -ef | grep -v grep | grep nginx | wc | awk '{print $1;exit}')
    # echo "nginx_num:$nginx_num"

    if [ $nginx_num -gt 0 ]; then
      msg="0,$(date '+%Y-%m-%d %H:%M:%S'),${CATALINA_LOG_FILE} $last_tomcat_log_end,nginx start"
      echo -e "\n$msg" && echo "$msg" >/home/admin/tomcatstat
      break
    elif ((retry == 100)); then
      # 超过100次，不再检查
      msg="1,$(date '+%Y-%m-%d %H:%M:%S'),'Started Application in' in ${CATALINA_LOG_FILE} $last_tomcat_log_end,WARNING: time out to find nginx"
      echo -e "\n$msg" && echo "$msg" >/home/admin/tomcatstat
      break
    else
      echo -n "."
      sleep 2s
      retry=$(($retry + 1))
    fi
  done
}

function check_error_in_log {
  local msg
  if [ -f "/home/admin/tomcat_check_key.md" ] && [ -s "/home/admin/tomcat_check_key.md" ]; then
    tomcat_check_keys=$(cat /home/admin/tomcat_check_key.md | tr '\n' '|')
  fi

  # echo "Error keys: ${tomcat_check_keys}"

  # 检查tomcat启动成功时是否异常
  errorMessage=$(tail -c +${last_tomcat_log_end} ${CATALINA_LOG_FILE} | head -n ${tomcat_startup_line} | grep -E -ni "${tomcat_check_keys}")

  # echo "Find errorMessage:${errorMessage}"

  if [ -n "$errorMessage" ]; then
    msg="1,$(date '+%Y-%m-%d %H:%M:%S'),find 'error' in ${CATALINA_LOG_FILE} $last_tomcat_log_end,$errorMessage"
    echo -e "\n$msg" && echo "$msg" >/home/admin/tomcatstat
    exit 1
  else
    if [ "$init_start_nginx" == "on" ]; then
      echo -e "\n Tomcat start success"
      start_nginx
      check_nginx
    else
      echo -e "\n0,$(date '+%Y-%m-%d %H:%M:%S'),${CATALINA_LOG_FILE} $last_tomcat_log_end" >/home/admin/tomcatstat
    fi

    exit 0
  fi
}

function check_log {

  local -i retry=0
  local msg

  #sleep 5s

  #每2s检查一次，最多360次，即12min
  while true; do

    # 当前日志所占字节
    current_Log=$(ls -l ${CATALINA_LOG_FILE} | awk '{print $5;exit}')
    # echo "current_Log:$current_Log"

    # 本次tomcat启动完成所占用的行数
    tomcat_startup_line=$(tail -c +${last_tomcat_log_end} ${CATALINA_LOG_FILE} | grep -ni "${tomcat_success_key}" | awk -F ':' '{print $1;exit}')

    # echo "tomcat_startup_line:${tomcat_startup_line}"

    if [ $tomcat_startup_line ]; then
      # 若Server startup 在新的tomcat日志中存在
      check_error_in_log
      break
    elif [ $retry -eq 100 ] && [ $current_Log == $last_tomcat_log_end ]; then
      # 五分钟之内没有新增日志，不再检查
      msg="1,$(date '+%Y-%m-%d %H:%M:%S'),WARNING: log not change to find '${tomcat_success_key}' in ${CATALINA_LOG_FILE} $last_tomcat_log_end"
      echo -e "\n$msg" && echo "$msg" >/home/admin/tomcatstat
      break
    elif ((retry == 400)); then
      # 超过20分钟，不再检查
      msg="1,$(date '+%Y-%m-%d %H:%M:%S'),WARNING: time out to find '${tomcat_success_key}' in ${CATALINA_LOG_FILE} $last_tomcat_log_end"
      echo -e "\n$msg" && echo "$msg" >/home/admin/tomcatstat
      break
    else
      echo -n "."
      sleep 2s
      retry=$(($retry + 1))
    fi
  done
}

check_log
